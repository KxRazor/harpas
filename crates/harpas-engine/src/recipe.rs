//! Crafting and smelting recipes from the vanilla data pack (`data/minecraft/recipe/`).
//!
//! The grid is trimmed to the box its items fill (`CraftingInput.ofPositioned`). A shaped recipe matches a grid of
//! its size whose every cell its pattern's ingredient accepts, as written or mirrored; a shapeless recipe matches a
//! grid with as many items as it has ingredients, each taken by a different ingredient. Recipes are tried in the
//! order of their IDs, and the first that matches crafts. Ingredients name items or item tags, which the data
//! pack's item tags resolve.
//!
//! Ported: `crafting_shaped` and `crafting_shapeless` recipes whose results have no data components. Not done yet:
//! the other crafting recipes (transmute, dye, special), results with components, and the recipe book.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read, Seek};
use std::path::Path;

use harpas_core::item::items;
use serde_json::Value;

use crate::furnace::fuel_times;
use crate::inventory::ItemStack;

/// The items an ingredient accepts (`Ingredient`, a set of items).
type Ingredient = HashSet<u32>;

#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Shaped {
        width: usize,
        height: usize,
        /// Row by row; `None` for an empty cell.
        cells: Vec<Option<Ingredient>>,
        symmetrical: bool,
    },
    Shapeless(Vec<Ingredient>),
}

#[derive(Debug, Clone, PartialEq)]
struct Recipe {
    shape: Shape,
    result: (u32, i32),
}

/// What a recipe leaves in the grid of each item it used (`craftRemainder`).
pub(crate) fn remainder(item: u32) -> Option<u32> {
    let name = items().name(item)?;
    let left = match name {
        "minecraft:milk_bucket"
        | "minecraft:water_bucket"
        | "minecraft:lava_bucket"
        | "minecraft:powder_snow_bucket" => "minecraft:bucket",
        "minecraft:honey_bottle" | "minecraft:dragon_breath" => "minecraft:glass_bottle",
        _ => return None,
    };
    items().id(left)
}

impl Recipe {
    fn matches(&self, width: usize, height: usize, grid: &[&ItemStack]) -> bool {
        let filled = grid.iter().filter(|s| !s.is_empty()).count();
        match &self.shape {
            Shape::Shaped {
                width: w,
                height: h,
                cells,
                symmetrical,
            } => {
                let needed = cells.iter().filter(|c| c.is_some()).count();
                if filled != needed || width != *w || height != *h {
                    return false;
                }
                let test = |mirrored: bool| {
                    (0..height).all(|y| {
                        (0..width).all(|x| {
                            let cell = if mirrored { w - x - 1 } else { x } + y * w;
                            let stack = grid[x + y * width];
                            match &cells[cell] {
                                None => stack.is_empty(),
                                Some(ingredient) => {
                                    !stack.is_empty() && ingredient.contains(&stack.item)
                                }
                            }
                        })
                    })
                };
                (!symmetrical && test(true)) || test(false)
            }
            Shape::Shapeless(ingredients) => {
                if filled != ingredients.len() {
                    return false;
                }
                let stacks: Vec<_> = grid.iter().filter(|s| !s.is_empty()).collect();
                // A perfect matching between the stacks and the ingredients (`StackedItemContents.canCraft`).
                let mut taken: Vec<Option<usize>> = vec![None; ingredients.len()];
                fn assign(
                    stack: usize,
                    stacks: &[&&ItemStack],
                    ingredients: &[Ingredient],
                    taken: &mut [Option<usize>],
                    seen: &mut [bool],
                ) -> bool {
                    for (i, ingredient) in ingredients.iter().enumerate() {
                        if seen[i] || !ingredient.contains(&stacks[stack].item) {
                            continue;
                        }
                        seen[i] = true;
                        if taken[i]
                            .is_none_or(|other| assign(other, stacks, ingredients, taken, seen))
                        {
                            taken[i] = Some(stack);
                            return true;
                        }
                    }
                    false
                }
                (0..stacks.len()).all(|stack| {
                    let mut seen = vec![false; ingredients.len()];
                    assign(stack, &stacks, ingredients, &mut taken, &mut seen)
                })
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SmeltingRecipe {
    pub(crate) id: String,
    pub(crate) result: ItemStack,
    pub(crate) ticks: i32,
}

/// Crafting and smelting recipes, in the order of their IDs.
#[derive(Debug, Default)]
pub struct Recipes {
    recipes: Vec<Recipe>,
    smelting: HashMap<u32, SmeltingRecipe>,
    smelting_experience: HashMap<String, f32>,
    fuels: HashMap<u32, i32>,
}

/// The item tags of a data pack, resolved to items.
fn resolve_tags(raw: &HashMap<String, Vec<String>>) -> HashMap<String, HashSet<u32>> {
    fn resolve(
        tag: &str,
        raw: &HashMap<String, Vec<String>>,
        done: &mut HashMap<String, HashSet<u32>>,
        active: &mut HashSet<String>,
    ) -> HashSet<u32> {
        if let Some(items) = done.get(tag) {
            return items.clone();
        }
        let mut found = HashSet::new();
        if !active.insert(tag.to_owned()) {
            return found;
        }
        for entry in raw.get(tag).into_iter().flatten() {
            match entry.strip_prefix('#') {
                Some(nested) => found.extend(resolve(nested, raw, done, active)),
                None => found.extend(items().id(entry)),
            }
        }
        active.remove(tag);
        done.insert(tag.to_owned(), found.clone());
        found
    }
    let mut done = HashMap::new();
    for tag in raw.keys() {
        resolve(tag, raw, &mut done, &mut HashSet::new());
    }
    done
}

fn ingredient(value: &Value, tags: &HashMap<String, HashSet<u32>>) -> Option<Ingredient> {
    let one = |name: &str| -> Option<Ingredient> {
        match name.strip_prefix('#') {
            Some(tag) => tags.get(tag).cloned(),
            None => items().id(name).map(|id| HashSet::from([id])),
        }
    };
    match value {
        Value::String(name) => one(name),
        Value::Array(names) => {
            let mut all = HashSet::new();
            for name in names {
                all.extend(one(name.as_str()?)?);
            }
            Some(all)
        }
        _ => None,
    }
}

impl Recipes {
    /// Reads the recipes and item tags of a data pack archive.
    pub fn from_zip_path(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_zip_reader(std::fs::File::open(path)?)
    }

    /// Reads the recipes and item tags of a data pack archive.
    pub fn from_zip_reader(reader: impl Read + Seek) -> io::Result<Self> {
        let mut archive = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
        let mut tags = HashMap::new();
        let mut recipes = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(io::Error::other)?;
            let name = entry.name().map_err(io::Error::other)?.to_string();
            let tag = name
                .strip_prefix("data/minecraft/tags/item/")
                .and_then(|n| n.strip_suffix(".json"));
            let recipe = name
                .strip_prefix("data/minecraft/recipe/")
                .and_then(|n| n.strip_suffix(".json"));
            if tag.is_none() && recipe.is_none() {
                continue;
            }
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            let json: Value = serde_json::from_str(&text).map_err(io::Error::other)?;
            if let Some(tag) = tag {
                let values = json
                    .get("values")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(|e| e.as_str().or_else(|| e.get("id")?.as_str()))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                tags.insert(format!("minecraft:{tag}"), values);
            } else if let Some(recipe) = recipe {
                recipes.insert(format!("minecraft:{recipe}"), json);
            }
        }
        Ok(Self::from_entries(
            &tags,
            recipes.iter().map(|(id, json)| (Some(id.as_str()), json)),
        ))
    }

    /// Builds the recipes from item tags (each a list of item names and `#` tags) and recipe JSON, in order.
    pub fn from_json<'a>(
        tags: &HashMap<String, Vec<String>>,
        recipes: impl IntoIterator<Item = &'a Value>,
    ) -> Self {
        Self::from_entries(tags, recipes.into_iter().map(|json| (None, json)))
    }

    fn from_entries<'a>(
        tags: &HashMap<String, Vec<String>>,
        recipes: impl IntoIterator<Item = (Option<&'a str>, &'a Value)>,
    ) -> Self {
        let tags = resolve_tags(tags);
        let mut parsed = Self {
            fuels: fuel_times(&tags),
            ..Self::default()
        };
        for (index, (id, json)) in recipes.into_iter().enumerate() {
            if json.get("type").and_then(Value::as_str) == Some("minecraft:smelting") {
                let id = id.map_or_else(|| format!("harpas:smelting/{index}"), str::to_owned);
                let Some((recipe, ingredients)) = Self::parse_smelting(id, json, &tags) else {
                    continue;
                };
                let experience = json.get("experience").map_or(Some(0.0), Value::as_f64);
                let Some(experience) = experience
                    .map(|value| value as f32)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                else {
                    continue;
                };
                parsed
                    .smelting_experience
                    .insert(recipe.id.clone(), experience);
                for item in ingredients {
                    parsed
                        .smelting
                        .entry(item)
                        .or_insert_with(|| recipe.clone());
                }
            } else if let Some(recipe) = Self::parse(json, &tags) {
                parsed.recipes.push(recipe);
            }
        }
        parsed
    }

    fn parse_smelting(
        id: String,
        json: &Value,
        tags: &HashMap<String, HashSet<u32>>,
    ) -> Option<(SmeltingRecipe, Ingredient)> {
        let result = json.get("result")?;
        if result.get("components").is_some() {
            return None;
        }
        let stack = ItemStack {
            item: items().id(result.get("id")?.as_str()?)?,
            count: i32::try_from(result.get("count").and_then(Value::as_i64).unwrap_or(1)).ok()?,
            ..ItemStack::default()
        };
        let ticks = i16::try_from(
            json.get("cookingtime")
                .and_then(Value::as_i64)
                .unwrap_or(200),
        )
        .ok()?;
        if ticks <= 0 || stack.is_empty() || !stack.is_valid() {
            return None;
        }
        let ingredients = ingredient(json.get("ingredient")?, tags)?;
        Some((
            SmeltingRecipe {
                id,
                result: stack,
                ticks: i32::from(ticks),
            },
            ingredients,
        ))
    }

    pub(crate) fn smelting(&self, stack: &ItemStack) -> Option<&SmeltingRecipe> {
        self.smelting.get(&stack.item).filter(|_| !stack.is_empty())
    }

    pub(crate) fn smelting_experience(&self, id: &str) -> Option<f32> {
        self.smelting_experience.get(id).copied().or_else(|| {
            (!id.contains(':'))
                .then(|| format!("minecraft:{id}"))
                .and_then(|id| self.smelting_experience.get(&id).copied())
        })
    }

    /// The number of ticks the stack's item fuels a furnace, or zero for an item that cannot burn.
    pub fn burn_duration(&self, stack: &ItemStack) -> i32 {
        if stack.is_empty() {
            0
        } else {
            self.fuels.get(&stack.item).copied().unwrap_or(0)
        }
    }

    fn parse(json: &Value, tags: &HashMap<String, HashSet<u32>>) -> Option<Recipe> {
        let result = json.get("result")?;
        if result.get("components").is_some() {
            return None;
        }
        let result = (
            items().id(result.get("id")?.as_str()?)?,
            result.get("count").and_then(Value::as_i64).unwrap_or(1) as i32,
        );
        let shape = match json.get("type")?.as_str()? {
            "minecraft:crafting_shaped" => {
                let rows: Vec<&str> = json
                    .get("pattern")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect();
                // `ShapedRecipePattern.shrink`: blank rows and columns at the edges go.
                let wide = rows.iter().map(|r| r.chars().count()).max()?;
                let used = |x: usize| {
                    rows.iter()
                        .any(|r| r.chars().nth(x).is_some_and(|c| c != ' '))
                };
                let first = (0..wide).find(|&x| used(x))?;
                let last = (0..wide).rev().find(|&x| used(x))?;
                let rows: Vec<Vec<char>> = rows
                    .iter()
                    .filter(|r| r.chars().any(|c| c != ' '))
                    .map(|r| {
                        (first..=last)
                            .map(|x| r.chars().nth(x).unwrap_or(' '))
                            .collect()
                    })
                    .collect();
                let key = json.get("key")?.as_object()?;
                let width = last - first + 1;
                let height = rows.len();
                let mut cells = Vec::with_capacity(width * height);
                for row in &rows {
                    for c in row {
                        if *c == ' ' {
                            cells.push(None);
                        } else {
                            cells.push(Some(ingredient(key.get(&c.to_string())?, tags)?));
                        }
                    }
                }
                let symmetrical = rows.iter().all(|row| row.iter().eq(row.iter().rev()));
                Shape::Shaped {
                    width,
                    height,
                    cells,
                    symmetrical,
                }
            }
            "minecraft:crafting_shapeless" => Shape::Shapeless(
                json.get("ingredients")?
                    .as_array()?
                    .iter()
                    .map(|i| ingredient(i, tags))
                    .collect::<Option<_>>()?,
            ),
            _ => return None,
        };
        Some(Recipe { shape, result })
    }

    /// The number of crafting recipes.
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// Whether there are no crafting recipes.
    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }

    /// What a crafting grid of `width` columns crafts, its stacks row by row (`getRecipeFor` and `assemble`).
    pub fn craft(&self, width: usize, grid: &[&ItemStack]) -> ItemStack {
        let height = grid.len() / width;
        let filled = |x: usize, y: usize| !grid[x + y * width].is_empty();
        let columns: Vec<usize> = (0..width)
            .filter(|&x| (0..height).any(|y| filled(x, y)))
            .collect();
        let rows: Vec<usize> = (0..height)
            .filter(|&y| (0..width).any(|x| filled(x, y)))
            .collect();
        let (Some(&left), Some(&right), Some(&top), Some(&bottom)) =
            (columns.first(), columns.last(), rows.first(), rows.last())
        else {
            return ItemStack::empty();
        };
        let (w, h) = (right - left + 1, bottom - top + 1);
        let trimmed: Vec<&ItemStack> = (top..=bottom)
            .flat_map(|y| (left..=right).map(move |x| grid[x + y * width]))
            .collect();
        self.recipes
            .iter()
            .find(|recipe| recipe.matches(w, h, &trimmed))
            .map_or_else(ItemStack::empty, |recipe| ItemStack {
                item: recipe.result.0,
                count: recipe.result.1,
                components: Default::default(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stack(name: &str) -> ItemStack {
        ItemStack {
            item: items().id(name).unwrap(),
            count: 1,
            components: Default::default(),
        }
    }

    fn recipes() -> Recipes {
        let tags = HashMap::from([
            (
                "minecraft:planks".to_owned(),
                vec![
                    "minecraft:oak_planks".to_owned(),
                    "minecraft:birch_planks".to_owned(),
                ],
            ),
            (
                "minecraft:oak_logs".to_owned(),
                vec!["minecraft:oak_log".to_owned()],
            ),
        ]);
        let recipes = [
            json!({"type": "minecraft:crafting_shapeless", "ingredients": ["#minecraft:oak_logs"],
                "result": {"id": "minecraft:oak_planks", "count": 4}}),
            json!({"type": "minecraft:crafting_shaped", "key": {"#": "#minecraft:planks"}, "pattern": ["#", "#"],
                "result": {"id": "minecraft:stick", "count": 4}}),
            json!({"type": "minecraft:crafting_shaped", "key": {"#": "#minecraft:planks"}, "pattern": ["##", "##"],
                "result": {"id": "minecraft:crafting_table"}}),
            json!({"type": "minecraft:crafting_shaped", "key": {"#": "minecraft:stick", "X": ["minecraft:coal", "minecraft:charcoal"]},
                "pattern": ["X ", "# "], "result": {"id": "minecraft:torch", "count": 4}}),
        ];
        Recipes::from_json(&tags, recipes.iter())
    }

    #[test]
    fn grids_craft_wherever_their_items_sit() {
        let recipes = recipes();
        let empty = ItemStack::empty();
        let log = stack("minecraft:oak_log");
        let crafted = |grid: [&ItemStack; 4]| {
            let made = recipes.craft(2, &grid);
            (made.name(), made.count)
        };
        assert_eq!(
            crafted([&empty, &empty, &empty, &log]),
            (Some("minecraft:oak_planks"), 4)
        );
        let oak = stack("minecraft:oak_planks");
        let birch = stack("minecraft:birch_planks");
        // A column of two planks of any kind, in either column.
        assert_eq!(
            crafted([&empty, &oak, &empty, &birch]),
            (Some("minecraft:stick"), 4)
        );
        assert_eq!(
            crafted([&oak, &oak, &oak, &oak]),
            (Some("minecraft:crafting_table"), 1)
        );
        // The torch's blank column is trimmed away.
        let coal = stack("minecraft:charcoal");
        let stick = stack("minecraft:stick");
        assert_eq!(
            crafted([&empty, &coal, &empty, &stick]),
            (Some("minecraft:torch"), 4)
        );
        assert!(recipes.craft(2, &[&oak, &empty, &empty, &oak]).is_empty());
        assert!(recipes.craft(2, &[&empty; 4]).is_empty());
    }
}
