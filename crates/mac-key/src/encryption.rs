use crate::{Error, Result};
use unicorn_engine::{
    unicorn_const::{Arch, Mode, Prot},
    RegisterX86, Unicorn,
};

include!(concat!(env!("OUT_DIR"), "/encryption_image.rs"));
const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/encrypt.dylib"));
const BASE: u64 = 0x100_0000;
const SCRATCH: u64 = 0x200_0000;
const STACK_SIZE: u64 = 0x10_0000;
const INPUT: u64 = SCRATCH + STACK_SIZE;
const OUTPUT: u64 = INPUT + 0x1000;
const RETURN: u64 = OUTPUT + 0x1000;

pub fn encrypt(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > 1024 {
        return Err(Error::new("Invalid hardware property length"));
    }
    // Run the unchanged upstream routine on either host architecture.
    let mut cpu = Unicorn::new(Arch::X86, Mode::MODE_64).map_err(emulation_error)?;
    // The upstream routine includes discarded reads just before its code image.
    cpu.mem_map(BASE - 0x10000, 0x10000, Prot::READ)
        .map_err(emulation_error)?;
    for &(address, size, offset, length) in SEGMENTS {
        cpu.mem_map(BASE + address, size.next_multiple_of(4096), Prot::ALL)
            .map_err(emulation_error)?;
        cpu.mem_write(
            BASE + address,
            &IMAGE[offset as usize..(offset + length) as usize],
        )
        .map_err(emulation_error)?;
    }
    cpu.mem_map(SCRATCH, STACK_SIZE + 0x3000, Prot::ALL)
        .map_err(emulation_error)?;
    cpu.mem_write(INPUT, data).map_err(emulation_error)?;
    let stack = SCRATCH + STACK_SIZE - 8;
    cpu.mem_write(stack, &RETURN.to_le_bytes())
        .map_err(emulation_error)?;
    for (register, value) in [
        (RegisterX86::RSP, stack),
        (RegisterX86::RDI, INPUT),
        (RegisterX86::RSI, data.len() as u64),
        (RegisterX86::RDX, OUTPUT),
    ] {
        cpu.reg_write(register, value).map_err(emulation_error)?;
    }
    cpu.emu_start(BASE + ENTRY, RETURN, 1_000_000, 10_000_000)
        .map_err(emulation_error)?;
    if cpu.reg_read(RegisterX86::RIP).map_err(emulation_error)? != RETURN {
        return Err(Error::new("Hardware encryption did not finish"));
    }
    cpu.mem_read_as_vec(OUTPUT, 17).map_err(emulation_error)
}

fn emulation_error(code: unicorn_engine::unicorn_const::uc_error) -> Error {
    Error::new(&format!(
        "Could not prepare encrypted hardware properties ({code:?})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};

    #[test]
    fn matches_upstream_encryption_vectors() {
        for (input, expected) in [
            (b"C02YT1YMJK7M".to_vec(), "c3kZ7+WofxcjaBTInJCwSV0="),
            (
                uuid::Uuid::parse_str("11D299A5-CF0B-544D-BAD3-7AC7A6E452D7")
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
                "jGguP3mQH+Vw6dMAWrqZOnk=",
            ),
            (
                uuid::Uuid::parse_str("FCDB63B5-D208-4AEE-B368-3DE952B911FF")
                    .unwrap()
                    .as_bytes()
                    .to_vec(),
                "VvJODAsuSRdGQlhB5kPgf2M=",
            ),
            (
                STANDARD.decode("V9BNndaG").unwrap(),
                "wWF12gciXzN/96bIt/ufTB0=",
            ),
            (b"C02923200KVKN3YAG".to_vec(), "CKp4ROiInBYAdvnbrbNjjkM="),
        ] {
            assert_eq!(STANDARD.encode(encrypt(&input).unwrap()), expected);
        }
    }

    #[test]
    fn handles_calls_from_multiple_threads() {
        let threads: Vec<_> = (0..4)
            .map(|_| std::thread::spawn(|| encrypt(b"C02YT1YMJK7M").unwrap()))
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(encrypt(&[]).is_err());
        assert!(encrypt(&[1; 1025]).is_err());
    }
}
