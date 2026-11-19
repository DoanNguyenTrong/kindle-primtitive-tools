//! The local time offset from a TZif file (`/etc/localtime`, which links to `/var/local/system/tz` on the
//! Kindle).
//!
//! The Kindle writes a TZif version 2 file with an empty POSIX footer. musl, which the launcher is built
//! with, uses that footer for every time after the last transition, and an empty footer means UTC: the clock
//! showed UTC. glibc (the stock UI, `date`) keeps the last transition's offset instead. This does the same:
//! the offset of the transition in force at `now`, from the version 1 data block (32-bit times, enough
//! until 2038).

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(data: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Seconds east of UTC at `now` (Unix seconds), or `None` when `data` is not a TZif file.
pub fn offset_at(data: &[u8], now: i64) -> Option<i32> {
    if data.get(0..4)? != b"TZif" {
        return None;
    }
    let timecnt = u32_at(data, 32)? as usize;
    let typecnt = u32_at(data, 36)? as usize;
    if typecnt == 0 {
        return None;
    }
    let times_at = 44;
    let index_at = times_at + timecnt * 4;
    let types_at = index_at + timecnt;
    // Before the first transition, type 0 applies (RFC 8536).
    let mut kind = 0usize;
    for i in 0..timecnt {
        if i64::from(i32_at(data, times_at + 4 * i)?) > now {
            break;
        }
        kind = *data.get(index_at + i)? as usize;
    }
    if kind >= typecnt {
        return None;
    }
    i32_at(data, types_at + 6 * kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 1 TZif block: `transitions` as (time, type index), `offsets` as the types' UTC offsets.
    fn tzif(transitions: &[(i32, u8)], offsets: &[i32]) -> Vec<u8> {
        let mut d = b"TZif2".to_vec();
        d.extend([0u8; 15]);
        let charcnt = 4u32;
        for count in [0u32, 0, 0, transitions.len() as u32, offsets.len() as u32, charcnt] {
            d.extend(count.to_be_bytes());
        }
        for (t, _) in transitions {
            d.extend(t.to_be_bytes());
        }
        for (_, k) in transitions {
            d.push(*k);
        }
        for off in offsets {
            d.extend(off.to_be_bytes());
            d.extend([0u8, 0u8]);
        }
        d.extend(b"ABC\0");
        d
    }

    #[test]
    fn the_last_transition_in_force_gives_the_offset() {
        let data = tzif(&[(0, 1), (1_000_000, 2)], &[0, 3600, 7 * 3600]);
        assert_eq!(offset_at(&data, -5), Some(0)); // before any transition: type 0
        assert_eq!(offset_at(&data, 500), Some(3600));
        assert_eq!(offset_at(&data, 1_794_935_000), Some(7 * 3600)); // long after the last one
    }

    #[test]
    fn a_file_without_transitions_uses_its_only_type() {
        assert_eq!(offset_at(&tzif(&[], &[7 * 3600]), 1_794_935_000), Some(7 * 3600));
    }

    #[test]
    fn other_data_is_not_a_zone() {
        assert_eq!(offset_at(b"GMT+7", 0), None);
        assert_eq!(offset_at(b"TZif2", 0), None);
        assert_eq!(offset_at(&tzif(&[(0, 5)], &[0]), 10), None);
    }
}
