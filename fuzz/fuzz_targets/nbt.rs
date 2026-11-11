//! NBT as packets and as files carry it. A value that reads must write and read back as the same bytes;
//! values are not compared, because a NaN differs from itself.
#![no_main]

use harpas_core::nbt::Nbt;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = Nbt::read_network(&mut &data[..]) {
        let mut written = Vec::new();
        value.write_network(&mut written);
        let mut again = Vec::new();
        Nbt::read_network(&mut &written[..])
            .expect("written NBT reads back")
            .write_network(&mut again);
        assert_eq!(again, written);
    }
    let _ = Nbt::read_network_optional(&mut &data[..]);
    if let Ok((name, value)) = Nbt::read_named(&mut &data[..]) {
        let mut written = Vec::new();
        value.write_named(&name, &mut written);
        let (name, value) = Nbt::read_named(&mut &written[..]).expect("written NBT reads back");
        let mut again = Vec::new();
        value.write_named(&name, &mut again);
        assert_eq!(again, written);
    }
});
