use num_bigint::BigUint;

pub fn format_to_human(raw_val: &str, decimals: u32) -> String {
    let numeric_str = if raw_val.starts_with("0x") || raw_val.starts_with("0X") {
        match BigUint::parse_bytes(
            raw_val
                .trim_start_matches("0x")
                .trim_start_matches("0X")
                .as_bytes(),
            16,
        ) {
            Some(v) => v.to_string(),
            None => return raw_val.to_string(),
        }
    } else {
        match raw_val.parse::<BigUint>() {
            Ok(v) => v.to_string(),
            Err(_) => return raw_val.to_string(),
        }
    };

    if numeric_str == "0" || decimals == 0 {
        return numeric_str;
    }

    let mut padded = numeric_str;
    while padded.len() <= decimals as usize {
        padded.insert(0, '0');
    }

    let split_idx = padded.len() - decimals as usize;
    let (integer_part, fractional_part) = padded.split_at(split_idx);

    let clean_fraction = fractional_part.trim_end_matches('0');
    if clean_fraction.is_empty() {
        integer_part.to_string()
    } else {
        format!("{}.{}", integer_part, clean_fraction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_eth_1() {
        assert_eq!(format_to_human("0xde0b6b3a7640000", 18), "1");
    }

    #[test]
    fn test_format_eth_2_5() {
        assert_eq!(format_to_human("0x22b1c8c1227a0000", 18), "2.5");
    }

    #[test]
    fn test_format_usdc_100() {
        assert_eq!(format_to_human("0x5f5e100", 6), "100");
    }

    #[test]
    fn test_format_sol_2_5() {
        assert_eq!(format_to_human("2500000000", 9), "2.5");
    }

    #[test]
    fn test_format_zero() {
        assert_eq!(format_to_human("0x0", 18), "0");
    }

    #[test]
    fn test_format_small_fraction() {
        assert_eq!(format_to_human("0x1", 18), "0.000000000000000001");
    }

    #[test]
    fn test_format_no_decimals() {
        assert_eq!(format_to_human("42", 0), "42");
    }

    #[test]
    fn test_format_usdc_50() {
        assert_eq!(format_to_human("0x2faf080", 6), "50");
    }

    #[test]
    fn test_format_large_value() {
        let one_million_usdc = 1_000_000_000_000u128;
        assert_eq!(format_to_human(&one_million_usdc.to_string(), 6), "1000000");
    }
}
