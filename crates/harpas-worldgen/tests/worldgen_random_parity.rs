//! Verifies the RNG wrapper used for structure decoration, including Xoroshiro's different bit consumption.
mod reference;

use harpas_core::random::{AnyRandom, LegacyRandom, RandomSource, WorldgenRandom, XoroshiroRandom};
use reference::Reference;

#[test]
fn worldgen_random_bit_sampling_matches_vanilla() {
    let Some(reference) = Reference::from_env() else {
        eprintln!("Skipping worldgen RNG: HARPAS_VANILLA_SERVER_DIR is not set");
        return;
    };
    reference.compile(&[(
        "WorldgenRandomProbe.java",
        include_str!("WorldgenRandomProbe.java"),
    )]);
    let output = reference.temp.0.join("worldgen-random.txt");
    reference.java("WorldgenRandomProbe", [output.display().to_string()]);
    let text = std::fs::read_to_string(output).unwrap();
    let mut expected = text.lines().map(|line| line.parse::<i64>().unwrap());
    for legacy in [true, false] {
        for seed in [0, 42, -1, i64::MIN] {
            let source = if legacy {
                AnyRandom::Legacy(LegacyRandom::new(seed))
            } else {
                AnyRandom::Xoroshiro(XoroshiroRandom::new(seed))
            };
            let mut random = WorldgenRandom::new(source);
            assert_eq!(
                Some(random.set_decoration_seed(seed, -32, 48)),
                expected.next(),
                "decoration: legacy={legacy}, seed={seed}"
            );
            random.set_feature_seed(seed, 0, 3);
            for _ in 0..128 {
                assert_eq!(Some(i64::from(random.next_int())), expected.next());
                for bound in [1, 16, 37, 1_500_000_000] {
                    assert_eq!(
                        Some(i64::from(random.next_int_bounded(bound))),
                        expected.next()
                    );
                }
                assert_eq!(Some(random.next_long()), expected.next());
                assert_eq!(Some(i64::from(random.next_bool())), expected.next());
                assert_eq!(
                    Some(i64::from(random.next_float().to_bits() as i32)),
                    expected.next()
                );
                assert_eq!(Some(random.next_double().to_bits() as i64), expected.next());
                assert_eq!(
                    Some(random.next_gaussian().to_bits() as i64),
                    expected.next()
                );
            }
            random.next_gaussian();
            random.set_seed(seed);
            assert_eq!(
                Some(random.next_gaussian().to_bits() as i64),
                expected.next(),
                "Gaussian cache after reseed: legacy={legacy}, seed={seed}"
            );
            assert_eq!(Some(random.next_long()), expected.next());
        }
    }
    assert!(expected.next().is_none());
}
