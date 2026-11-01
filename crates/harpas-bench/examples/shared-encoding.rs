//! Shared encoding prototype: encode and compress a chunk column once and serve it to many connections.
//!
//! Answers the *(measure)* questions in `docs/protocol-codecs.md` (CFB8 cost, zlib implementation) and
//! `docs/bedrock-interop.md` (shared compression, hashed block IDs).

use std::hint::black_box;

use aes::cipher::array::Array;
use aes::cipher::{BlockCipherEncrypt, BlockModeEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::{Aes128, Aes256};
use harpas_bench::chunk::{self, BedrockIds, Column};
use harpas_bench::measure::{micros, time};
use sha2::{Digest, Sha256};

type JavaCipher = cfb8::Encryptor<Aes128>;
type BedrockCipher = ctr::Ctr32BE<Aes256>;

const VIEWERS: [usize; 5] = [1, 2, 10, 50, 200];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// zlib header and checksum, as Java uses.
    Zlib,
    /// Raw DEFLATE, as Bedrock uses.
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Library {
    MinizOxide,
    ZlibRs,
    Libdeflate,
}

impl Library {
    const ALL: [Library; 3] = [Library::MinizOxide, Library::ZlibRs, Library::Libdeflate];

    fn name(self) -> &'static str {
        match self {
            Library::MinizOxide => "miniz_oxide",
            Library::ZlibRs => "zlib-rs",
            Library::Libdeflate => "libdeflate",
        }
    }
}

/// A compressor with its reusable state, as one worker thread would keep it.
struct Compressor {
    library: Library,
    level: i32,
    libdeflate: libdeflater::Compressor,
    out: Vec<u8>,
}

impl Compressor {
    fn new(library: Library, level: i32) -> Compressor {
        let lvl = libdeflater::CompressionLvl::new(level).expect("valid level");
        Compressor {
            library,
            level,
            libdeflate: libdeflater::Compressor::new(lvl),
            out: Vec::new(),
        }
    }

    fn compress(&mut self, input: &[u8], format: Format) -> &[u8] {
        match self.library {
            Library::MinizOxide => {
                let level = self.level as u8;
                self.out = match format {
                    Format::Zlib => miniz_oxide::deflate::compress_to_vec_zlib(input, level),
                    Format::Raw => miniz_oxide::deflate::compress_to_vec(input, level),
                };
                &self.out
            }
            Library::ZlibRs => {
                let mut config = zlib_rs::DeflateConfig::new(self.level);
                if format == Format::Raw {
                    config.window_bits = -15;
                }
                self.out.resize(zlib_rs::compress_bound(input.len()), 0);
                let (written, code) = zlib_rs::compress_slice(&mut self.out, input, config);
                assert_eq!(code, zlib_rs::ReturnCode::Ok);
                let len = written.len();
                &self.out[..len]
            }
            Library::Libdeflate => {
                let bound = match format {
                    Format::Zlib => self.libdeflate.zlib_compress_bound(input.len()),
                    Format::Raw => self.libdeflate.deflate_compress_bound(input.len()),
                };
                self.out.resize(bound, 0);
                let len = match format {
                    Format::Zlib => self.libdeflate.zlib_compress(input, &mut self.out),
                    Format::Raw => self.libdeflate.deflate_compress(input, &mut self.out),
                }
                .expect("buffer is large enough");
                &self.out[..len]
            }
        }
    }
}

fn write_varint(buf: &mut Vec<u8>, value: usize) {
    let mut value = value as u32;
    while value >= 0x80 {
        buf.push(value as u8 | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// A Java connection: the frame it receives is encrypted with its own continuous CFB8 stream.
struct JavaConnection {
    cipher: JavaCipher,
    out: Vec<u8>,
}

impl JavaConnection {
    fn new(seed: u8) -> JavaConnection {
        let key = [seed; 16];
        JavaConnection {
            cipher: JavaCipher::new(&key.into(), &key.into()),
            out: Vec::new(),
        }
    }

    /// Copies a compressed frame into the connection's buffer and encrypts it there.
    fn send(&mut self, frame: &[u8]) {
        self.out.clear();
        self.out.extend_from_slice(frame);
        let (blocks, rest) = Array::slice_as_chunks_mut(&mut self.out);
        debug_assert!(rest.is_empty());
        self.cipher.encrypt_blocks(blocks);
        black_box(&self.out);
    }
}

/// CFB8 written directly on the AES block function, to compare with the `cfb8` crate.
struct ManualCfb8 {
    aes: Aes128,
    register: [u8; 16],
}

impl ManualCfb8 {
    fn new(key: [u8; 16]) -> ManualCfb8 {
        ManualCfb8 {
            aes: Aes128::new(&key.into()),
            register: key,
        }
    }

    fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let mut block = Array::from(self.register);
            self.aes.encrypt_block(&mut block);
            *byte ^= block[0];
            self.register.copy_within(1.., 0);
            self.register[15] = *byte;
        }
    }
}

/// Frames a Java packet with compression enabled: packet length, uncompressed length, zlib data.
fn java_frame(packet: &[u8], compressed: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(compressed.len() + 5);
    write_varint(&mut inner, packet.len());
    inner.extend_from_slice(compressed);
    let mut frame = Vec::with_capacity(inner.len() + 5);
    write_varint(&mut frame, inner.len());
    frame.extend_from_slice(&inner);
    frame
}

/// A Bedrock connection: checksum over its send counter, then its own AES-256-CTR stream.
struct BedrockConnection {
    key: [u8; 32],
    counter: u64,
    cipher: BedrockCipher,
    out: Vec<u8>,
}

impl BedrockConnection {
    fn new(seed: u8) -> BedrockConnection {
        let key = [seed; 32];
        let mut iv = [0u8; 16];
        iv[..12].copy_from_slice(&key[..12]);
        iv[15] = 2;
        BedrockConnection {
            key,
            counter: 0,
            cipher: BedrockCipher::new(&key.into(), &iv.into()),
            out: Vec::new(),
        }
    }

    /// Copies a compressed batch, appends its checksum and encrypts both.
    fn send(&mut self, batch: &[u8]) {
        let checksum = Sha256::new()
            .chain_update(self.counter.to_le_bytes())
            .chain_update(batch)
            .chain_update(self.key)
            .finalize();
        self.counter += 1;
        self.out.clear();
        self.out.push(0xfe);
        self.out.extend_from_slice(batch);
        self.out.extend_from_slice(&checksum[..8]);
        self.cipher.apply_keystream(&mut self.out[1..]);
        black_box(&self.out);
    }
}

fn main() {
    let column = Column::generate(3, -7);
    let mut java = Vec::new();
    chunk::encode_java_column(&mut java, &column);
    let mut bedrock = Vec::new();
    chunk::encode_bedrock_batch(&mut bedrock, &column, BedrockIds::Hashed);
    let mut bedrock_runtime = Vec::new();
    chunk::encode_bedrock_batch(&mut bedrock_runtime, &column, BedrockIds::Runtime);

    println!("## Column\n");
    let palettes: Vec<String> = column
        .sections
        .iter()
        .map(|s| s.palette().len().to_string())
        .collect();
    println!(
        "Palette sizes by section, bottom to top: {}\n",
        palettes.join(" ")
    );
    println!("| Encoding | Bytes |\n|---|---|");
    println!(
        "| Java Chunk Data and Update Light, uncompressed | {} |",
        java.len()
    );
    println!(
        "| Bedrock sub-chunk batch, runtime IDs | {} |",
        bedrock_runtime.len()
    );
    println!(
        "| Bedrock sub-chunk batch, hashed IDs | {} |",
        bedrock.len()
    );
    let mut zlib = Compressor::new(Library::ZlibRs, 6);
    println!(
        "| Java, compressed with zlib-rs level 6 | {} |",
        zlib.compress(&java, Format::Zlib).len()
    );
    println!(
        "| Bedrock, runtime IDs, compressed | {} |",
        zlib.compress(&bedrock_runtime, Format::Raw).len()
    );
    println!(
        "| Bedrock, hashed IDs, compressed | {} |",
        zlib.compress(&bedrock, Format::Raw).len()
    );

    println!("\n## Encoding, single thread\n");
    println!("| Step | µs |\n|---|---|");
    let java_encode = time(|| {
        let mut buf = Vec::with_capacity(java.len());
        chunk::encode_java_column(&mut buf, black_box(&column));
        black_box(buf);
    });
    println!("| Java column | {} |", micros(java_encode));
    let bedrock_encode = time(|| {
        let mut buf = Vec::with_capacity(bedrock.len());
        chunk::encode_bedrock_batch(&mut buf, black_box(&column), BedrockIds::Hashed);
        black_box(buf);
    });
    println!("| Bedrock column | {} |", micros(bedrock_encode));

    println!("\n## Compression\n");
    println!(
        "| Library | Level | Java µs | Java bytes | Bedrock µs | Bedrock bytes |\n|---|---|---|---|---|---|"
    );
    for library in Library::ALL {
        for level in [1, 4, 6] {
            let mut compressor = Compressor::new(library, level);
            let java_bytes = compressor.compress(&java, Format::Zlib).len();
            let java_ns = time(|| {
                black_box(compressor.compress(black_box(&java), Format::Zlib));
            });
            let bedrock_bytes = compressor.compress(&bedrock, Format::Raw).len();
            let bedrock_ns = time(|| {
                black_box(compressor.compress(black_box(&bedrock), Format::Raw));
            });
            println!(
                "| {} | {level} | {} | {java_bytes} | {} | {bedrock_bytes} |",
                library.name(),
                micros(java_ns),
                micros(bedrock_ns)
            );
        }
    }

    println!("\n## Encryption, per connection\n");
    let mut compressor = Compressor::new(Library::Libdeflate, 6);
    let frame = java_frame(&java, compressor.compress(&java, Format::Zlib));
    let bedrock_batch = compressor.compress(&bedrock, Format::Raw).to_vec();
    let mut java_conn = JavaConnection::new(1);
    let cfb8 = time(|| java_conn.send(black_box(&frame)));
    let mut manual = ManualCfb8::new([1; 16]);
    let mut check = frame.clone();
    manual.encrypt(&mut check);
    java_conn = JavaConnection::new(1);
    java_conn.send(&frame);
    assert_eq!(check, java_conn.out, "manual CFB8 matches the cfb8 crate");
    let mut out = frame.clone();
    let manual_ns = time(|| {
        out.copy_from_slice(&frame);
        manual.encrypt(black_box(&mut out));
    });
    let mut bedrock_conn = BedrockConnection::new(1);
    let ctr = time(|| bedrock_conn.send(black_box(&bedrock_batch)));
    let rate = |bytes: usize, ns: f64| format!("{:.0}", bytes as f64 / ns * 1e9 / 1e6);
    println!("| Cipher | Bytes | µs | MB/s |\n|---|---|---|---|");
    println!(
        "| Java AES-128-CFB8 | {} | {} | {} |",
        frame.len(),
        micros(cfb8),
        rate(frame.len(), cfb8)
    );
    println!(
        "| Java AES-128-CFB8, on the block function | {} | {} | {} |",
        frame.len(),
        micros(manual_ns),
        rate(frame.len(), manual_ns)
    );
    println!(
        "| Bedrock SHA-256 checksum + AES-256-CTR | {} | {} | {} |",
        bedrock_batch.len(),
        micros(ctr),
        rate(bedrock_batch.len(), ctr)
    );

    println!("\n## Cost per viewer: encode and compress per viewer vs. once, zlib-rs level 6\n");
    println!(
        "| Viewers | Java per viewer µs | Java shared µs | Java shared, unencrypted µs | Bedrock per viewer µs | \
         Bedrock shared µs |"
    );
    println!("|---|---|---|---|---|---|");
    let mut compressor = zlib;
    for viewers in VIEWERS {
        let mut java_conns: Vec<_> = (0..viewers).map(|i| JavaConnection::new(i as u8)).collect();
        let mut bedrock_conns: Vec<_> = (0..viewers)
            .map(|i| BedrockConnection::new(i as u8))
            .collect();
        let java_each = time(|| {
            for conn in &mut java_conns {
                let mut packet = Vec::with_capacity(java.len());
                chunk::encode_java_column(&mut packet, &column);
                let frame = java_frame(&packet, compressor.compress(&packet, Format::Zlib));
                conn.send(&frame);
            }
        });
        let java_shared = time(|| {
            let mut packet = Vec::with_capacity(java.len());
            chunk::encode_java_column(&mut packet, &column);
            let frame = java_frame(&packet, compressor.compress(&packet, Format::Zlib));
            for conn in &mut java_conns {
                conn.send(&frame);
            }
        });
        let mut plain: Vec<Vec<u8>> = vec![Vec::new(); viewers];
        let java_plain = time(|| {
            let mut packet = Vec::with_capacity(java.len());
            chunk::encode_java_column(&mut packet, &column);
            let frame = java_frame(&packet, compressor.compress(&packet, Format::Zlib));
            for out in &mut plain {
                out.clear();
                out.extend_from_slice(&frame);
                black_box(&out);
            }
        });
        let bedrock_each = time(|| {
            for conn in &mut bedrock_conns {
                let mut batch = Vec::with_capacity(bedrock.len());
                chunk::encode_bedrock_batch(&mut batch, &column, BedrockIds::Hashed);
                conn.send(compressor.compress(&batch, Format::Raw));
            }
        });
        let bedrock_shared = time(|| {
            let mut batch = Vec::with_capacity(bedrock.len());
            chunk::encode_bedrock_batch(&mut batch, &column, BedrockIds::Hashed);
            let compressed = compressor.compress(&batch, Format::Raw).to_vec();
            for conn in &mut bedrock_conns {
                conn.send(&compressed);
            }
        });
        let per = |ns: f64| micros(ns / viewers as f64);
        println!(
            "| {viewers} | {} | {} | {} | {} | {} |",
            per(java_each),
            per(java_shared),
            per(java_plain),
            per(bedrock_each),
            per(bedrock_shared)
        );
    }
}
