//! Jigsaw replacements compared through vanilla's complete template processing pipeline.
mod reference;

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;

use harpas_core::block::block_states;
use harpas_core::nbt::{Compound, List, Nbt};
use harpas_core::random::{RandomSource, XoroshiroRandom};
use harpas_worldgen::WorldgenData;
use harpas_worldgen::processor::{ProcessorBlock, ProcessorPipeline};
use reference::Reference;
use serde_json::{Value, json};

fn state(name: &str) -> u32 {
    block_states()
        .default_state(&format!("minecraft:{name}"))
        .unwrap()
}

fn replacement() -> Value {
    json!({"processor_type": "minecraft:jigsaw_replacement"})
}

fn rule(output: &str, modifier: Value) -> Value {
    json!({"processor_type": "minecraft:rule", "rules": [{
        "input_predicate": {"predicate_type": "minecraft:always_true"},
        "location_predicate": {"predicate_type": "minecraft:always_true"},
        "output_state": {"Name": format!("minecraft:{output}")},
        "block_entity_modifier": modifier
    }]})
}

fn tag(text: Option<&str>) -> (Value, Option<Compound>) {
    let Some(text) = text else {
        return (Value::Null, None);
    };
    (
        json!(format!(
            "{{final_state:{},keep:{{n:7}},LootTable:'test:old'}}",
            json!(text)
        )),
        Some(
            [
                ("final_state", Nbt::String(text.into())),
                (
                    "keep",
                    Nbt::Compound([("n", Nbt::Int(7))].into_iter().collect()),
                ),
                ("LootTable", Nbt::String("test:old".into())),
            ]
            .into_iter()
            .collect(),
        ),
    )
}

fn snapshot(blocks: Vec<ProcessorBlock>, random: Option<i64>) -> Compound {
    let blocks: Vec<Compound> = blocks
        .into_iter()
        .map(|block| {
            let mut out: Compound = [
                ("pos", Nbt::IntArray(block.pos.to_vec())),
                ("state", Nbt::Int(block.state as i32)),
            ]
            .into_iter()
            .collect();
            if let Some(nbt) = block.nbt {
                out.insert("nbt", nbt);
            }
            out
        })
        .collect();
    let mut out: Compound = [(
        "blocks",
        Nbt::List(if blocks.is_empty() {
            List::Empty
        } else {
            List::Compound(blocks)
        }),
    )]
    .into_iter()
    .collect();
    if let Some(random) = random {
        out.insert("random", random);
    }
    out
}

fn compare(
    reference: &Reference,
    pipelines: Vec<Vec<Value>>,
    cases: Vec<Value>,
    expected: Vec<Compound>,
) {
    let fixtures = reference.temp.0.join("jigsaw.json");
    fs::write(
        &fixtures,
        json!({"pipelines": pipelines, "cases": cases}).to_string(),
    )
    .unwrap();
    reference.compile(&[
        (
            "StructurePlacementProbe.java",
            include_str!("StructurePlacementProbe.java"),
        ),
        (
            "RuleProcessorProbe.java",
            include_str!("RuleProcessorProbe.java"),
        ),
        (
            "CappedProcessorProbe.java",
            include_str!("CappedProcessorProbe.java"),
        ),
        (
            "FilterProcessorProbe.java",
            include_str!("FilterProcessorProbe.java"),
        ),
    ]);
    let output = reference.temp.0.join("jigsaw.nbt");
    reference.java(
        "FilterProcessorProbe",
        [fixtures.display().to_string(), output.display().to_string()],
    );
    let bytes = fs::read(output).unwrap();
    let (_, Nbt::Compound(root)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
        panic!("output compound")
    };
    let Some(Nbt::List(List::Compound(actual))) = root.get("results") else {
        panic!("output results")
    };
    assert_eq!(actual.len(), expected.len());
    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        if actual != expected {
            let records = |tag: &Compound| match tag.get("blocks") {
                Some(Nbt::List(List::Compound(blocks))) => blocks.clone(),
                _ => vec![],
            };
            let a = records(actual);
            let e = records(expected);
            let differences: Vec<_> = a
                .iter()
                .filter(|b| !e.contains(b))
                .chain(e.iter().filter(|b| !a.contains(b)))
                .take(6)
                .collect();
            let inputs: Vec<_> = cases[index]["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|input| {
                    differences.iter().any(|b| {
                        b.get("pos")
                            == Some(&Nbt::IntArray(
                                input[0]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| v.as_i64().unwrap() as i32)
                                    .collect(),
                            ))
                    })
                })
                .collect();
            panic!(
                "case {index}, pipeline {}: different blocks {differences:?}; inputs {inputs:?}; random {:?} / {:?}",
                cases[index]["pipeline"],
                actual.get("random"),
                expected.get("random")
            );
        }
    }
    eprintln!("{} jigsaw processing cases match vanilla", cases.len());
}

fn texts() -> Vec<String> {
    let mut texts: Vec<String> = [
        "",
        "stone",
        ":stone",
        "minecraft:stone",
        "structure_void",
        "air",
        "cave_air",
        "void_air",
        "jigsaw[orientation=up_east]",
        "chest[facing=west,waterlogged=true]",
        "oak_log[axis=x]",
        "oak_log[]",
        "oak_log[axis=x,]",
        "oak_log[ 'axis' = \"x\" ]",
        "oak_log [axis=x]",
        "oak_log[axis=x]suffix",
        "stone garbage",
        "stone{",
        "stone{}trailing",
        "stone {invalid",
        " stone",
        "STONE",
        "#minecraft:logs",
        "minecraft:",
        "unknown",
        "test:stone",
        "minecraft:stone:bad",
        "stone[bad=x]",
        "oak_log[axis=bad]",
        "oak_log[axis=x,axis=y]",
        "oak_log[axis=x,,]",
        "oak_log[axis=x",
        "oak_log[axis='x]",
        "oak_log[axis=\"x\\n\"]",
        "oak_log[axis=x\u{a0}]",
        "oak_log[\u{1c}axis=x]",
        "stone{a:bool(1,)}",
        "stone{a:bool('true')}",
        "stone{a:uuid('1-1-1-1-1')}",
        "stone{a:uuid('invalid')}",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for value in [
        "true",
        "false",
        "True",
        "TRUE",
        "bool(True)",
        "bool([])",
        "bool({})",
        "bool(0)",
        "bool(-.5f)",
        "bool(true,)",
        "bool()",
        "bool(1,2)",
        "unknown(1)",
        "null",
        "NaN",
        "foo-bar",
        "foo.bar",
        "foo+bar",
        "minecraft:stone",
        "01",
        "01f",
        "0.5",
        "1.",
        ".5",
        "+.5",
        "1e3",
        "1e999",
        "1e999f",
        "1 e3",
        "128b",
        "-129b",
        "127b",
        "-128b",
        "255ub",
        "256ub",
        "-1ub",
        "32768s",
        "65535us",
        "2147483648",
        "9223372036854775808L",
        "18446744073709551615ul",
        "0xffffffff",
        "-0xffffffff",
        "-0x80000000si",
        "0b101",
        "0xCAFE",
        "0x11ub",
        "0x11b",
        "1_000",
        "1_",
        "1__0",
        "1_2.3_4__5f",
        "1sf",
        "1u",
        "[1,'a',{},]",
        "[B;1,-128,255ub]",
        "[ B ; 1 ]",
        "[B;128]",
        "[B;true]",
        "[I;1b,2s,3]",
        "[I;1L]",
        "[L;1.0]",
        "[I;'x']",
        "[i;1]",
        "[,1]",
        "[1,,]",
        "{a:1,}",
        "{1:2}",
        "{,}",
        "{a:}",
        r#"'\x42\uD800\U0001F600\N{Snowman}'"#,
        r#"'\N{NULL}'"#,
        r#"'\N{not a character}'"#,
        r#"'\N{snow man}'"#,
        r#"'\U00110000'"#,
        r#"'\q'"#,
        r#"'\x0'"#,
        r#"'\u000Z'"#,
    ] {
        texts.push(format!("stone{{a:{value}}}"));
    }
    texts
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn jigsaw_final_states_and_ordered_pipelines_match_vanilla() {
    let reference = Reference::required();
    let data = WorldgenData::from_zip_path(&reference.server_jar).unwrap();
    let jigsaw = replacement();
    let mut pipelines = vec![vec![jigsaw.clone()], vec![jigsaw.clone(), jigsaw.clone()]];
    for other in [
        json!({"processor_type":"minecraft:block_ignore","blocks":[{"Name":"minecraft:air"},{"Name":"minecraft:jigsaw"}]}),
        json!({"processor_type":"minecraft:block_rot","integrity":0.5}),
        json!({"processor_type":"minecraft:protected_blocks","value":"#minecraft:features_cannot_replace"}),
        rule("stone", json!({"type":"minecraft:passthrough"})),
        rule(
            "jigsaw",
            json!({"type":"minecraft:append_static", "data":{"final_state":"gold_block"}}),
        ),
        rule("jigsaw", json!({"type":"minecraft:clear"})),
        json!({"processor_type":"minecraft:capped","limit":3,"delegate":rule("jigsaw", json!({"type":"minecraft:append_static", "data":{"final_state":"structure_void"}}))}),
    ] {
        pipelines.push(vec![jigsaw.clone(), other.clone()]);
        pipelines.push(vec![other, jigsaw.clone()]);
    }
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    let texts = texts();
    for (index, entries) in pipelines.iter().enumerate() {
        let pipeline = ProcessorPipeline::compile("parity", entries, &data)
            .unwrap()
            .unwrap();
        for &seed in reference::sampled_seeds(&[0, -91, 0x1234_5678_abcd_ef01]) {
            for origin in [[0, 0, 0], [-17, -64, 32], [29_999_984, 319, -29_999_984]] {
                for explicit in [false, true] {
                    let mut inputs = Vec::new();
                    let mut blocks = Vec::new();
                    // Invalid final states on other blocks must never be parsed or filtered.
                    for name in ["jigsaw", "stone", "structure_void"] {
                        for text in [None]
                            .into_iter()
                            .chain(texts.iter().map(|s| Some(s.as_str())))
                        {
                            let (input, nbt) = tag(text);
                            let i = blocks.len() as i32;
                            let pos = [origin[0] + i, origin[1] + i % 3, origin[2] - i];
                            inputs.push(json!([
                                pos,
                                state(name),
                                input,
                                state(if i % 3 == 0 { "bedrock" } else { "air" })
                            ]));
                            blocks.push(ProcessorBlock {
                                pos,
                                state: state(name),
                                nbt,
                            });
                        }
                    }
                    for value in [
                        None,
                        Some(Nbt::Int(7)),
                        Some(Nbt::Compound(Compound::new())),
                    ] {
                        let mut nbt = Compound::new();
                        let input = match value {
                            None => "{}",
                            Some(Nbt::Int(_)) => "{final_state:7}",
                            _ => "{final_state:{}}",
                        };
                        if let Some(value) = value {
                            nbt.insert("final_state", value);
                        }
                        let i = blocks.len() as i32;
                        let pos = [origin[0] + i, origin[1] + i % 3, origin[2] - i];
                        inputs.push(json!([
                            pos,
                            state("jigsaw"),
                            input,
                            state(if i % 3 == 0 { "bedrock" } else { "air" })
                        ]));
                        blocks.push(ProcessorBlock {
                            pos,
                            state: state("jigsaw"),
                            nbt: Some(nbt),
                        });
                    }
                    let mut random = XoroshiroRandom::new(seed);
                    let output = pipeline.process(
                        seed,
                        origin,
                        [origin[0] + 7, origin[1] - 3, origin[2] + 2],
                        &blocks,
                        |pos| {
                            state(if (pos[0] - origin[0]) % 3 == 0 {
                                "bedrock"
                            } else {
                                "air"
                            })
                        },
                        explicit.then_some(&mut random),
                    );
                    expected.push(snapshot(output, explicit.then(|| random.next_long())));
                    cases.push(json!({"pipeline":index,"seed":seed,"origin":origin,"random":explicit,"blocks":inputs}));
                }
            }
        }
    }
    compare(&reference, pipelines, cases, expected);
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn every_registry_state_can_be_a_jigsaw_final_state() {
    let reference = Reference::required();
    let data = WorldgenData::default();
    let entries = vec![replacement()];
    let pipeline = ProcessorPipeline::compile("parity", &entries, &data)
        .unwrap()
        .unwrap();
    let registry = block_states();
    let mut inputs = Vec::new();
    let mut blocks = Vec::new();
    for id in 0..registry.len() {
        let (name, properties) = registry.properties(id).unwrap();
        let text = format!(
            "{name}[{}]",
            properties
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (input, nbt) = tag(Some(&text));
        let pos = [id as i32, 64, -17];
        inputs.push(json!([pos, state("jigsaw"), input, 0]));
        blocks.push(ProcessorBlock {
            pos,
            state: state("jigsaw"),
            nbt,
        });
    }
    let output = pipeline.process(
        0,
        [0; 3],
        [7, -3, 2],
        &blocks,
        |_| panic!("replacement read terrain"),
        None,
    );
    // An independent expectation prevents matching parser bugs from turning this into a round trip only.
    assert_eq!(output.len(), registry.len() as usize - 1);
    for block in &output {
        assert_eq!(block.state, block.pos[0] as u32);
        assert!(block.nbt.is_none());
    }
    compare(
        &reference,
        vec![entries],
        vec![json!({"pipeline":0,"seed":0,"origin":[0,0,0],"random":false,"blocks":inputs})],
        vec![snapshot(output, None)],
    );
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn attached_snbt_and_shipped_template_final_states_match_vanilla() {
    let reference = Reference::required();
    let mut texts: BTreeSet<String> = texts().into_iter().collect();
    for literal in [
        "-1",
        "0",
        "00",
        "1",
        "127",
        "128",
        "255",
        "256",
        "32767",
        "32768",
        "65535",
        "65536",
        "2147483647",
        "2147483648",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
        "0xffffffff",
        "0x100000000",
        "0x80000000",
        "0xffffffffffffffff",
        "0b101",
        "-0x1",
        "+1",
        "1.5",
        "1.",
        ".5",
        "1e2",
        "1e+2",
        "1e-2",
        "01f",
        "1_0",
        "1__0",
        "1_",
        "_1",
        "1 2",
        "1 . 5",
        "1 e + 3",
        "0 xFF",
    ] {
        for suffix in [
            "", "b", "s", "i", "l", "f", "d", "sb", "ss", "si", "sl", "ub", "us", "ui", "ul", "UB",
            "u", "sf",
        ] {
            let number = format!("{literal}{suffix}");
            for value in [
                number.clone(),
                format!("[B;{number}]"),
                format!("[I;{number}]"),
                format!("[L;{number}]"),
            ] {
                texts.insert(format!("stone{{a:{value}}}"));
            }
        }
    }
    for name in [
        "NULL",
        "NUL",
        "BEL",
        "BELL",
        "LINE FEED",
        "LINE FEED (LF)",
        "PADDING CHARACTER",
        "LATIN 1 SUPPLEMENT 84",
        "SNOWMAN",
        "snowman ",
        "snow man",
        "GRINNING FACE",
        "LATIN SMALL LETTER A",
        "BASIC LATIN 41",
        "CJK UNIFIED IDEOGRAPH-4E00",
        "CJK UNIFIED IDEOGRAPHS 4E00",
        "CJK UNIFIED IDEOGRAPHS EXTENSION A 3400",
        "HANGUL SYLLABLE GA",
        "HANGUL SYLLABLES AC00",
        "HIGH SURROGATES D800",
        "HIGH PRIVATE USE SURROGATES DB80",
        "LOW SURROGATES DC00",
        "PRIVATE USE AREA E000",
        "SUPPLEMENTARY PRIVATE USE AREA A F0000",
        "SUPPLEMENTARY PRIVATE USE AREA B 100000",
        "TANGUT 17000",
        "TANGUT SUPPLEMENT 18D00",
    ] {
        texts.insert(format!("stone{{a:'\\N{{{name}}}'}}"));
    }
    // Game data stays in the owner's jar. Include the actual final states from every template,
    // in addition to synthetic command states and malformed suffixes.
    let mut archive = zip::ZipArchive::new(fs::File::open(&reference.server_jar).unwrap()).unwrap();
    let mut templates = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().unwrap();
        if !name.starts_with("data/minecraft/structure/") || !name.ends_with(".nbt") {
            continue;
        }
        templates += 1;
        let mut compressed = Vec::new();
        entry.read_to_end(&mut compressed).unwrap();
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut bytes)
            .unwrap();
        let (_, Nbt::Compound(root)) = Nbt::read_named(&mut bytes.as_slice()).unwrap() else {
            panic!("template compound")
        };
        if let Some(Nbt::List(List::Compound(blocks))) = root.get("blocks") {
            for block in blocks {
                if let Some(Nbt::Compound(nbt)) = block.get("nbt")
                    && let Some(Nbt::String(text)) = nbt.get("final_state")
                {
                    texts.insert(text.clone());
                }
            }
        }
    }
    assert_eq!(templates, 1202);
    let entries = vec![replacement()];
    let pipeline = ProcessorPipeline::compile("snbt parity", &entries, &WorldgenData::default())
        .unwrap()
        .unwrap();
    let mut inputs = Vec::new();
    let mut blocks = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        let (input, nbt) = tag(Some(text));
        let pos = [i as i32, 64, -17];
        inputs.push(json!([pos, state("jigsaw"), input, 0]));
        blocks.push(ProcessorBlock {
            pos,
            state: state("jigsaw"),
            nbt,
        });
    }
    let output = pipeline.process(
        0,
        [0; 3],
        [7, -3, 2],
        &blocks,
        |_| panic!("replacement read terrain"),
        None,
    );
    compare(
        &reference,
        vec![entries],
        vec![json!({"pipeline":0,"seed":0,"origin":[0,0,0],"random":false,"blocks":inputs})],
        vec![snapshot(output, None)],
    );
    eprintln!(
        "{} command strings, including final states from {templates} templates",
        texts.len()
    );
}
