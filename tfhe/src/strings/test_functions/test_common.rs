use crate::shortint::parameters::PARAM_MESSAGE_2_CARRY_2;
use crate::strings::ciphertext::{ClearString, FheString, GenericPattern};
use crate::strings::server_key::{FheStringIsEmpty, FheStringLen};
use crate::strings::test_functions::{
    result_message, result_message_clear_pat, result_message_clear_rhs, result_message_pat,
    result_message_rhs,
};
use crate::strings::Keys;
use std::time::{Duration, Instant};

#[test]
fn test_len_is_empty() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str in ["", "a", "abc"] {
        for pad in 0..3 {
            keys.assert_len(str, Some(pad));
            keys.assert_is_empty(str, Some(pad));
        }
    }
}

#[test]
fn test_encrypt_decrypt() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str in ["", "a", "abc"] {
        for pad in 0..3 {
            keys.assert_encrypt_decrypt(str, Some(pad));
        }
    }
}

#[test]
fn test_strip() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str_pad in 0..2 {
        for pat_pad in 0..2 {
            for pat in ["", "a", "abc"] {
                for str in ["", "a", "abc", "b", "ab", "dddabc", "abceeee", "dddabceee"] {
                    keys.assert_strip_prefix(str, Some(str_pad), pat, Some(pat_pad));
                    keys.assert_strip_suffix(str, Some(str_pad), pat, Some(pat_pad));
                }
            }
        }
    }
}

const TEST_CASES_COMP: [&str; 5] = ["", "a", "aa", "ab", "abc"];

#[test]
fn test_comparisons() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str_pad in 0..2 {
        for rhs_pad in 0..2 {
            for str in TEST_CASES_COMP {
                for rhs in TEST_CASES_COMP {
                    keys.assert_comp(str, Some(str_pad), rhs, Some(rhs_pad));
                }
            }
        }
    }
}

impl Keys {
    pub fn assert_len(&self, str: &str, str_pad: Option<u32>) {
        let expected = str.len();

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let start = Instant::now();
        let result = self.sk.len(&enc_str);
        let end = Instant::now();

        let dec = match result {
            FheStringLen::NoPadding(clear_len) => clear_len,
            FheStringLen::Padding(enc_len) => self.ck.decrypt_radix::<u32>(&enc_len) as usize,
        };

        println!("\n\x1b[1mLen:\x1b[0m");
        result_message(str, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_is_empty(&self, str: &str, str_pad: Option<u32>) {
        let expected = str.is_empty();

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let start = Instant::now();
        let result = self.sk.is_empty(&enc_str);
        let end = Instant::now();

        let dec = match result {
            FheStringIsEmpty::NoPadding(clear_len) => clear_len,
            FheStringIsEmpty::Padding(enc_len) => self.ck.decrypt_bool(&enc_len),
        };

        println!("\n\x1b[1mIs_empty:\x1b[0m");
        result_message(str, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_encrypt_decrypt(&self, str: &str, str_pad: Option<u32>) {
        let enc_str = FheString::new(&self.ck, str, str_pad);

        let dec = self.ck.decrypt_ascii(&enc_str);

        println!("\n\x1b[1mEncrypt/Decrypt:\x1b[0m");
        result_message(str, str, &dec, Duration::from_nanos(0));

        assert_eq!(str, &dec);
    }

    pub fn assert_strip_prefix(
        &self,
        str: &str,
        str_pad: Option<u32>,
        pat: &str,
        pat_pad: Option<u32>,
    ) {
        let expected = str.strip_prefix(pat);

        let enc_str = FheString::new(&self.ck, str, str_pad);
        let enc_pat = GenericPattern::Enc(FheString::new(&self.ck, pat, pat_pad));
        let clear_pat = GenericPattern::Clear(ClearString::new(pat.to_string()));

        let start = Instant::now();
        let (result, is_some) = self.sk.strip_prefix(&enc_str, &enc_pat);
        let end = Instant::now();

        let dec_result = self.ck.decrypt_ascii(&result);
        let dec_is_some = self.ck.decrypt_bool(&is_some);
        if !dec_is_some {
            // When it's None, the FheString returned is the original str
            assert_eq!(dec_result, str);
        }

        let dec = dec_is_some.then_some(dec_result.as_str());

        println!("\n\x1b[1mStrip_prefix:\x1b[0m");
        result_message_pat(str, pat, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);

        let start = Instant::now();
        let (result, is_some) = self.sk.strip_prefix(&enc_str, &clear_pat);
        let end = Instant::now();

        let dec_result = self.ck.decrypt_ascii(&result);
        let dec_is_some = self.ck.decrypt_bool(&is_some);
        if !dec_is_some {
            // When it's None, the FheString returned is the original str
            assert_eq!(dec_result, str);
        }

        let dec = dec_is_some.then_some(dec_result.as_str());

        println!("\n\x1b[1mStrip_prefix:\x1b[0m");
        result_message_clear_pat(str, pat, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_strip_suffix(
        &self,
        str: &str,
        str_pad: Option<u32>,
        pat: &str,
        pat_pad: Option<u32>,
    ) {
        let expected = str.strip_suffix(pat);

        let enc_str = FheString::new(&self.ck, str, str_pad);
        let enc_pat = GenericPattern::Enc(FheString::new(&self.ck, pat, pat_pad));
        let clear_pat = GenericPattern::Clear(ClearString::new(pat.to_string()));

        let start = Instant::now();
        let (result, is_some) = self.sk.strip_suffix(&enc_str, &enc_pat);
        let end = Instant::now();

        let dec_result = self.ck.decrypt_ascii(&result);
        let dec_is_some = self.ck.decrypt_bool(&is_some);
        if !dec_is_some {
            // When it's None, the FheString returned is the original str
            assert_eq!(dec_result, str);
        }

        let dec = dec_is_some.then_some(dec_result.as_str());

        println!("\n\x1b[1mStrip_suffix:\x1b[0m");
        result_message_pat(str, pat, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);

        let start = Instant::now();
        let (result, is_some) = self.sk.strip_suffix(&enc_str, &clear_pat);
        let end = Instant::now();

        let dec_result = self.ck.decrypt_ascii(&result);
        let dec_is_some = self.ck.decrypt_bool(&is_some);
        if !dec_is_some {
            // When it's None, the FheString returned is the original str
            assert_eq!(dec_result, str);
        }

        let dec = dec_is_some.then_some(dec_result.as_str());

        println!("\n\x1b[1mStrip_suffix:\x1b[0m");
        result_message_clear_pat(str, pat, expected, dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_comp(&self, str: &str, str_pad: Option<u32>, rhs: &str, rhs_pad: Option<u32>) {
        let enc_lhs = FheString::new(&self.ck, str, str_pad);
        let enc_rhs = GenericPattern::Enc(FheString::new(&self.ck, rhs, rhs_pad));
        let clear_rhs = GenericPattern::Clear(ClearString::new(rhs.to_string()));

        // Equal
        let expected_eq = str == rhs;

        let start = Instant::now();
        let result_eq = self.sk.eq(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_eq = self.ck.decrypt_bool(&result_eq);

        println!("\n\x1b[1mEq:\x1b[0m");
        result_message_rhs(str, rhs, expected_eq, dec_eq, end.duration_since(start));
        assert_eq!(dec_eq, expected_eq);

        // Clear rhs
        let start = Instant::now();
        let result_eq = self.sk.eq(&enc_lhs, &clear_rhs);
        let end = Instant::now();

        let dec_eq = self.ck.decrypt_bool(&result_eq);

        println!("\n\x1b[1mEq:\x1b[0m");
        result_message_clear_rhs(str, rhs, expected_eq, dec_eq, end.duration_since(start));
        assert_eq!(dec_eq, expected_eq);

        // Not equal
        let expected_ne = str != rhs;

        let start = Instant::now();
        let result_ne = self.sk.ne(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_ne = self.ck.decrypt_bool(&result_ne);

        println!("\n\x1b[1mNe:\x1b[0m");
        result_message_rhs(str, rhs, expected_ne, dec_ne, end.duration_since(start));
        assert_eq!(dec_ne, expected_ne);

        // Clear rhs
        let start = Instant::now();
        let result_ne = self.sk.ne(&enc_lhs, &clear_rhs);
        let end = Instant::now();

        let dec_ne = self.ck.decrypt_bool(&result_ne);

        println!("\n\x1b[1mNe:\x1b[0m");
        result_message_clear_rhs(str, rhs, expected_ne, dec_ne, end.duration_since(start));
        assert_eq!(dec_ne, expected_ne);

        let enc_rhs = FheString::new(&self.ck, rhs, rhs_pad);

        // Greater or equal
        let expected_ge = str >= rhs;

        let start = Instant::now();
        let result_ge = self.sk.ge(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_ge = self.ck.decrypt_bool(&result_ge);

        println!("\n\x1b[1mGe:\x1b[0m");
        result_message_rhs(str, rhs, expected_ge, dec_ge, end.duration_since(start));
        assert_eq!(dec_ge, expected_ge);

        // Less or equal
        let expected_le = str <= rhs;

        let start = Instant::now();
        let result_le = self.sk.le(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_le = self.ck.decrypt_bool(&result_le);

        println!("\n\x1b[1mLe:\x1b[0m");
        result_message_rhs(str, rhs, expected_le, dec_le, end.duration_since(start));
        assert_eq!(dec_le, expected_le);

        // Greater than
        let expected_gt = str > rhs;

        let start = Instant::now();
        let result_gt = self.sk.gt(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_gt = self.ck.decrypt_bool(&result_gt);

        println!("\n\x1b[1mGt:\x1b[0m");
        result_message_rhs(str, rhs, expected_gt, dec_gt, end.duration_since(start));
        assert_eq!(dec_gt, expected_gt);

        // Less than
        let expected_lt = str < rhs;

        let start = Instant::now();
        let result_lt = self.sk.lt(&enc_lhs, &enc_rhs);
        let end = Instant::now();

        let dec_lt = self.ck.decrypt_bool(&result_lt);

        println!("\n\x1b[1mLt:\x1b[0m");
        result_message_rhs(str, rhs, expected_lt, dec_lt, end.duration_since(start));
        assert_eq!(dec_lt, expected_lt);
    }
}