//! The normal furnace ticks with its loaded chunk. Its block entity owns the slots and timers.

use std::collections::{HashMap, HashSet};
use std::io;

use harpas_core::block::block_states;
use harpas_core::item::items;
use harpas_core::nbt::{Compound, List, Nbt};

use crate::block::BlockPos;
use crate::container_storage::{new_tag, read_items, write_items};
use crate::inventory::ItemStack;
use crate::item_data::invalid;
use crate::recipe::{Recipes, SmeltingRecipe, remainder};
use crate::world::World;

pub(crate) const FURNACE: &str = "minecraft:furnace";
pub(crate) const FURNACE_SLOTS: usize = 3;
pub(crate) const INPUT: usize = 0;
pub(crate) const FUEL: usize = 1;
pub(crate) const RESULT: usize = 2;
const DEFAULT_COOKING_TIME: i32 = 200;
const TIMER_KEYS: [&str; 4] = [
    "lit_time_remaining",
    "lit_total_time",
    "cooking_time_spent",
    "cooking_total_time",
];

/// Burn times in ticks, with material families resolved from the installed pack's item tags.
/// The parity probe compares this table with all of Vanilla 26.1's fuel values.
pub(crate) fn fuel_times(tags: &HashMap<String, HashSet<u32>>) -> HashMap<u32, i32> {
    let families: &[(i32, &[&str])] = &[
        (67, &["wool_carpets"]),
        (100, &["wool", "saplings", "wooden_buttons"]),
        (150, &["wooden_slabs"]),
        (200, &["wooden_doors", "signs"]),
        (
            300,
            &[
                "logs_that_burn",
                "planks",
                "wooden_stairs",
                "wooden_pressure_plates",
                "wooden_trapdoors",
                "wooden_fences",
                "fence_gates",
                "banners",
                "wooden_shelves",
            ],
        ),
        (800, &["hanging_signs"]),
        (1200, &["boats", "chest_boats"]),
        (1600, &["coals"]),
    ];
    let individual: &[(i32, &[&str])] = &[
        (50, &["bamboo", "scaffolding"]),
        (
            100,
            &[
                "bowl",
                "dead_bush",
                "leaf_litter",
                "short_dry_grass",
                "stick",
                "tall_dry_grass",
            ],
        ),
        (150, &["bamboo_mosaic_slab"]),
        (
            200,
            &[
                "wooden_axe",
                "wooden_hoe",
                "wooden_pickaxe",
                "wooden_shovel",
                "wooden_spear",
                "wooden_sword",
            ],
        ),
        (
            300,
            &[
                "bamboo_block",
                "bamboo_mosaic",
                "bamboo_mosaic_stairs",
                "barrel",
                "bookshelf",
                "bow",
                "cartography_table",
                "chest",
                "chiseled_bookshelf",
                "composter",
                "crafting_table",
                "crossbow",
                "daylight_detector",
                "fishing_rod",
                "fletching_table",
                "jukebox",
                "ladder",
                "lectern",
                "loom",
                "mangrove_roots",
                "note_block",
                "smithing_table",
                "stripped_bamboo_block",
                "trapped_chest",
            ],
        ),
        (1600, &["coal", "charcoal"]),
        (2400, &["blaze_rod"]),
        (4001, &["dried_kelp_block"]),
        (16000, &["coal_block"]),
        (20000, &["lava_bucket"]),
    ];
    let mut fuels = HashMap::new();
    for (ticks, names) in families {
        for name in *names {
            if let Some(tag) = tags.get(&format!("minecraft:{name}")) {
                fuels.extend(tag.iter().map(|&item| (item, *ticks)));
            }
        }
    }
    if let Some(excluded) = tags.get("minecraft:non_flammable_wood") {
        fuels.retain(|item, _| !excluded.contains(item));
    }
    for (ticks, names) in individual {
        fuels.extend(names.iter().filter_map(|name| {
            items()
                .id(&format!("minecraft:{name}"))
                .map(|item| (item, *ticks))
        }));
    }
    fuels
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Furnace {
    pub(crate) slots: [ItemStack; FURNACE_SLOTS],
    remaining: i32,
    duration: i32,
    progress: i32,
    total: i32,
    recipes_used: Compound,
}

impl Furnace {
    pub(crate) fn properties(&self) -> [i16; 4] {
        [self.remaining, self.duration, self.progress, self.total].map(|value| value as i16)
    }

    pub(crate) fn set_slots(
        &mut self,
        slots: [ItemStack; FURNACE_SLOTS],
        recipes: Option<&Recipes>,
    ) {
        let old = &self.slots[INPUT];
        let input = &slots[INPUT];
        if input.item != old.item || input.components != old.components {
            self.total = recipes
                .and_then(|r| r.smelting(input))
                .map_or(DEFAULT_COOKING_TIME, |r| r.ticks);
            self.progress = 0;
        }
        self.slots = slots;
    }

    fn fits(&self, recipe: &SmeltingRecipe) -> bool {
        let result = &recipe.result;
        let output = &self.slots[RESULT];
        output.is_empty()
            || (output.item == result.item
                && output.components == result.components
                && output.count + result.count <= output.max_stack_size().unwrap_or(0))
    }

    fn needs_tick(&self, recipes: &Recipes) -> bool {
        self.remaining > 0
            || self.progress > 0
            || (recipes.burn_duration(&self.slots[FUEL]) > 0
                && recipes
                    .smelting(&self.slots[INPUT])
                    .is_some_and(|recipe| self.fits(recipe)))
    }

    fn tick(&mut self, recipes: &Recipes) {
        self.remaining = (self.remaining - 1).max(0);
        if self.remaining == 0 && (self.slots[FUEL].is_empty() || self.slots[INPUT].is_empty()) {
            self.progress = (self.progress - 2).max(0);
            return;
        }
        let recipe = recipes
            .smelting(&self.slots[INPUT])
            .filter(|recipe| self.fits(recipe));
        if let Some(recipe) = recipe {
            if self.remaining == 0 {
                self.remaining = recipes.burn_duration(&self.slots[FUEL]);
                self.duration = self.remaining;
                if self.remaining > 0 {
                    let fuel = &mut self.slots[FUEL];
                    let left = remainder(fuel.item);
                    fuel.count -= 1;
                    if fuel.is_empty() {
                        *fuel = left.map_or_else(ItemStack::empty, single);
                    }
                }
            }
            if self.remaining > 0 {
                if self.total == 0 {
                    self.total = recipe.ticks;
                }
                self.progress += 1;
                if self.progress >= self.total {
                    self.progress = 0;
                    self.total = recipe.ticks;
                    self.finish(recipe);
                }
                return;
            }
        }
        self.progress = 0;
    }

    fn finish(&mut self, recipe: &SmeltingRecipe) {
        if self.slots[RESULT].is_empty() {
            self.slots[RESULT] = recipe.result.clone();
        } else {
            self.slots[RESULT].count += recipe.result.count;
        }
        if self.slots[INPUT].name() == Some("minecraft:wet_sponge")
            && self.slots[FUEL].name() == Some("minecraft:bucket")
            && let Some(water) = items().id("minecraft:water_bucket")
        {
            self.slots[FUEL] = single(water);
        }
        self.slots[INPUT].count -= 1;
        if self.slots[INPUT].is_empty() {
            self.slots[INPUT] = ItemStack::empty();
        }
        if let Some(id) = &recipe.id {
            let used = match self.recipes_used.get(id) {
                Some(Nbt::Int(n)) => n.saturating_add(1),
                _ => 1,
            };
            self.recipes_used.insert(id.as_str(), used);
        }
    }
}

fn single(item: u32) -> ItemStack {
    ItemStack {
        item,
        count: 1,
        ..ItemStack::default()
    }
}

impl World {
    pub(crate) fn furnace(&self, pos: BlockPos) -> io::Result<Furnace> {
        let Some(tag) = self.container_tag(pos, FURNACE)? else {
            return Ok(Furnace::default());
        };
        let timer = |key| match tag.get(key) {
            Some(Nbt::Short(value)) => i32::from(*value).max(0),
            _ => 0,
        };
        let [remaining, duration, progress, total] = TIMER_KEYS.map(timer);
        let recipes_used = match tag.get("RecipesUsed") {
            None => Compound::default(),
            Some(Nbt::Compound(used)) => used.clone(),
            _ => return Err(invalid("invalid furnace recipe usage")),
        };
        Ok(Furnace {
            slots: read_items(Some(tag), self.block_item_codec())?,
            remaining,
            duration,
            progress,
            total,
            recipes_used,
        })
    }

    pub(crate) fn save_furnace(&mut self, pos: BlockPos, furnace: &Furnace) -> io::Result<()> {
        let tag = self.furnace_with_items(pos, self.container_tag(pos, FURNACE)?, furnace)?;
        self.store_container_tag(pos, tag);
        if self
            .recipes
            .as_ref()
            .is_some_and(|recipes| furnace.needs_tick(recipes))
        {
            self.active_furnaces.insert(pos);
        } else {
            self.active_furnaces.remove(&pos);
        }
        Ok(())
    }

    pub(crate) fn furnace_with_items(
        &self,
        pos: BlockPos,
        tag: Option<&Compound>,
        furnace: &Furnace,
    ) -> io::Result<Compound> {
        let mut tag = tag.cloned().unwrap_or_else(|| new_tag(pos, FURNACE));
        tag.insert(
            "Items",
            Nbt::List(write_items(&furnace.slots, self.block_item_codec())?),
        );
        for (key, value) in TIMER_KEYS.into_iter().zip(furnace.properties()) {
            tag.insert(key, value);
        }
        tag.insert("RecipesUsed", Nbt::Compound(furnace.recipes_used.clone()));
        Ok(tag)
    }

    pub(crate) fn tick_furnace(
        &mut self,
        pos: BlockPos,
        recipes: &Recipes,
    ) -> io::Result<Option<Furnace>> {
        let Some(state) = self
            .block_state(pos)
            .filter(|&state| block_states().name(state) == Some(FURNACE))
        else {
            self.active_furnaces.remove(&pos);
            return Ok(None);
        };
        let mut furnace = self.furnace(pos)?;
        let was_lit = furnace.remaining > 0;
        let before = furnace.clone();
        furnace.tick(recipes);
        if furnace != before {
            self.save_furnace(pos, &furnace)?;
        } else if !furnace.needs_tick(recipes) {
            self.active_furnaces.remove(&pos);
        }
        let lit = furnace.remaining > 0;
        if was_lit != lit {
            let updated = crate::place::with(state, &[("lit", if lit { "true" } else { "false" })]);
            self.set_block_state(pos, updated);
        }
        Ok((furnace != before).then_some(furnace))
    }

    pub(crate) fn load_furnaces(&mut self, chunk: crate::world::ChunkPos) {
        let Some(recipes) = self.recipes.as_deref() else {
            return;
        };
        let Some(crate::world::ChunkState::Loaded(data)) = self.chunk(chunk) else {
            return;
        };
        let Some(Nbt::List(List::Compound(entities))) = data.rest.get("block_entities") else {
            return;
        };
        let active: Vec<_> = entities
            .iter()
            .filter_map(|tag| {
                if !matches!(tag.get("id"), Some(Nbt::String(id)) if id == FURNACE) {
                    return None;
                }
                let (Some(Nbt::Int(x)), Some(Nbt::Int(y)), Some(Nbt::Int(z))) =
                    (tag.get("x"), tag.get("y"), tag.get("z"))
                else {
                    return None;
                };
                let pos = BlockPos {
                    x: *x,
                    y: *y,
                    z: *z,
                };
                (pos.chunk() == chunk
                    && self
                        .furnace(pos)
                        .is_ok_and(|furnace| furnace.needs_tick(recipes)))
                .then_some(pos)
            })
            .collect();
        self.active_furnaces.extend(active);
    }
}
