/// Fast bitwise XOR operations on byte slices, optimized for SIMD auto-vectorization.

/// Performs `dst[i] ^= src[i]` for all `i` in `0..len`.
/// Assumes `dst.len() == src.len()`.
#[inline]
pub fn xor_inplace(dst: &mut [u8], src: &[u8]) {
    assert_eq!(dst.len(), src.len(), "Slice length mismatch in xor_inplace");

    let len = dst.len();
    let mut i = 0;

    // Fast path: Process 8 bytes (u64) at a time
    while i + 8 <= len {
        let dst_ptr = dst[i..i + 8].as_mut_ptr() as *mut u64;
        let src_ptr = src[i..i + 8].as_ptr() as *const u64;
        unsafe {
            let dst_val = dst_ptr.read_unaligned();
            let src_val = src_ptr.read_unaligned();
            dst_ptr.write_unaligned(dst_val ^ src_val);
        }
        i += 8;
    }

    // Process remaining trailing bytes
    while i < len {
        dst[i] ^= src[i];
        i += 1;
    }
}

/// Performs `dst[i] = src_a[i] ^ src_b[i]` for all `i` in `0..len`.
#[inline]
pub fn xor_into(dst: &mut [u8], src_a: &[u8], src_b: &[u8]) {
    assert_eq!(dst.len(), src_a.len(), "Slice length mismatch in xor_into");
    assert_eq!(dst.len(), src_b.len(), "Slice length mismatch in xor_into");

    let len = dst.len();
    let mut i = 0;

    while i + 8 <= len {
        let dst_ptr = dst[i..i + 8].as_mut_ptr() as *mut u64;
        let a_ptr = src_a[i..i + 8].as_ptr() as *const u64;
        let b_ptr = src_b[i..i + 8].as_ptr() as *const u64;
        unsafe {
            let a_val = a_ptr.read_unaligned();
            let b_val = b_ptr.read_unaligned();
            dst_ptr.write_unaligned(a_val ^ b_val);
        }
        i += 8;
    }

    while i < len {
        dst[i] = src_a[i] ^ src_b[i];
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xor_inplace() {
        let mut a = vec![0b10101010u8; 128];
        let b = vec![0b01010101u8; 128];
        xor_inplace(&mut a, &b);
        assert!(a.iter().all(|&byte| byte == 0b11111111));
    }
}
