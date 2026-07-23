use std::{io, time::Instant};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct Guid([u8; 16]);

impl Guid {
    pub(super) const ZERO: Self = Self([0; 16]);

    pub(super) const fn from_disk_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub(super) fn from_disk_slice(bytes: &[u8]) -> io::Result<Self> {
        let bytes: [u8; 16] = bytes
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "GPT GUID has an invalid width"))?;
        Ok(Self(bytes))
    }

    pub(super) fn parse_canonical(value: &str, deadline: Instant) -> io::Result<Self> {
        require_deadline(deadline)?;
        let bytes = value.as_bytes();
        if bytes.len() != 36 || bytes[8] != b'-' || bytes[13] != b'-' || bytes[18] != b'-' || bytes[23] != b'-' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected partition UUID is not canonical lowercase UUID text",
            ));
        }
        let mut canonical = [0_u8; 16];
        let groups = [(0, 8), (9, 13), (14, 18), (19, 23), (24, 36)];
        let mut output = 0usize;
        for (start, end) in groups {
            let mut index = start;
            while index < end {
                require_deadline(deadline)?;
                canonical[output] = (hex(bytes[index])? << 4) | hex(bytes[index + 1])?;
                output += 1;
                index += 2;
            }
        }
        let disk = [
            canonical[3],
            canonical[2],
            canonical[1],
            canonical[0],
            canonical[5],
            canonical[4],
            canonical[7],
            canonical[6],
            canonical[8],
            canonical[9],
            canonical[10],
            canonical[11],
            canonical[12],
            canonical[13],
            canonical[14],
            canonical[15],
        ];
        require_deadline(deadline)?;
        Ok(Self(disk))
    }

    pub(super) const fn is_zero(self) -> bool {
        self.0[0] == 0
            && self.0[1] == 0
            && self.0[2] == 0
            && self.0[3] == 0
            && self.0[4] == 0
            && self.0[5] == 0
            && self.0[6] == 0
            && self.0[7] == 0
            && self.0[8] == 0
            && self.0[9] == 0
            && self.0[10] == 0
            && self.0[11] == 0
            && self.0[12] == 0
            && self.0[13] == 0
            && self.0[14] == 0
            && self.0[15] == 0
    }

    pub(super) const fn canonical_bytes(self) -> [u8; 36] {
        let canonical = [
            self.0[3], self.0[2], self.0[1], self.0[0], self.0[5], self.0[4], self.0[7], self.0[6], self.0[8],
            self.0[9], self.0[10], self.0[11], self.0[12], self.0[13], self.0[14], self.0[15],
        ];
        let mut output = [0_u8; 36];
        let mut source = 0usize;
        let mut target = 0usize;
        while source < canonical.len() {
            if target == 8 || target == 13 || target == 18 || target == 23 {
                output[target] = b'-';
                target += 1;
            }
            output[target] = hexadecimal(canonical[source] >> 4);
            output[target + 1] = hexadecimal(canonical[source] & 0x0f);
            source += 1;
            target += 2;
        }
        output
    }
}

fn hex(byte: u8) -> io::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected partition UUID is not canonical lowercase UUID text",
        )),
    }
}

const fn hexadecimal(value: u8) -> u8 {
    if value < 10 { b'0' + value } else { b'a' + value - 10 }
}

fn require_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "GPT GUID parsing exceeded its deadline",
        ))
    } else {
        Ok(())
    }
}
