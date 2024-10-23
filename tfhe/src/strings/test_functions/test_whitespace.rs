use crate::shortint::parameters::PARAM_MESSAGE_2_CARRY_2;
use crate::strings::ciphertext::FheString;
use crate::strings::server_key::{split_ascii_whitespace, FheStringIterator};
use crate::strings::test_functions::result_message;
use crate::strings::Keys;
use std::time::Instant;
const WHITESPACES: [&str; 5] = [" ", "\n", "\t", "\r", "\u{000C}"];

#[test]
fn test_trim() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str_pad in 0..2 {
        for ws in WHITESPACES {
            for core in ["", "a", "a a"] {
                #[allow(clippy::useless_format)]
                for str in [
                    format!("{core}"),
                    format!("{ws}{core}"),
                    format!("{core}{ws}"),
                    format!("{ws}{core}{ws}"),
                ] {
                    keys.assert_trim(&str, Some(str_pad));
                    keys.assert_trim_start(&str, Some(str_pad));
                    keys.assert_trim_end(&str, Some(str_pad));
                }
            }
        }
    }
}

#[test]
fn test_split_ascii_whitespace() {
    let keys = Keys::new(PARAM_MESSAGE_2_CARRY_2);

    for str_pad in 0..2 {
        for ws in WHITESPACES {
            #[allow(clippy::useless_format)]
            for str in [
                format!(""),
                format!("{ws}"),
                format!("a{ws}"),
                format!("{ws}a"),
                format!("a{ws}a"),
                format!("{ws}{ws}"),
                format!("a{ws}{ws}"),
                format!("{ws}a{ws}"),
                format!("{ws}{ws}a"),
                format!("a{ws}a{ws}"),
                format!("a{ws}{ws}a"),
                format!("{ws}a{ws}a"),
                format!("a{ws}a{ws}a"),
            ] {
                keys.assert_split_ascii_whitespace(&str, Some(str_pad));
            }
        }
    }
}

impl Keys {
    pub fn assert_trim_end(&self, str: &str, str_pad: Option<u32>) {
        let expected = str.trim_end();

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let start = Instant::now();
        let result = self.sk.trim_end(&enc_str);
        let end = Instant::now();

        let dec = self.ck.decrypt_ascii(&result);

        println!("\n\x1b[1mTrim_end:\x1b[0m");
        result_message(str, expected, &dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_trim_start(&self, str: &str, str_pad: Option<u32>) {
        let expected = str.trim_start();

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let start = Instant::now();
        let result = self.sk.trim_start(&enc_str);
        let end = Instant::now();

        let dec = self.ck.decrypt_ascii(&result);

        println!("\n\x1b[1mTrim_start:\x1b[0m");
        result_message(str, expected, &dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_trim(&self, str: &str, str_pad: Option<u32>) {
        let expected = str.trim();

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let start = Instant::now();
        let result = self.sk.trim(&enc_str);
        let end = Instant::now();

        let dec = self.ck.decrypt_ascii(&result);

        println!("\n\x1b[1mTrim:\x1b[0m");
        result_message(str, expected, &dec, end.duration_since(start));

        assert_eq!(dec, expected);
    }

    pub fn assert_split_ascii_whitespace(&self, str: &str, str_pad: Option<u32>) {
        let mut expected: Vec<_> = str.split_ascii_whitespace().map(Some).collect();
        expected.push(None);

        let enc_str = FheString::new(&self.ck, str, str_pad);

        let mut results = Vec::with_capacity(expected.len());

        // Call next enough times
        let start = Instant::now();
        let mut split_iter = split_ascii_whitespace(&enc_str);
        for _ in 0..expected.len() {
            results.push(split_iter.next(&self.sk))
        }
        let end = Instant::now();

        // Collect the decrypted results properly
        let dec: Vec<_> = results
            .iter()
            .map(|(result, is_some)| {
                let dec_is_some = self.ck.decrypt_bool(is_some);
                let dec_result = self.ck.decrypt_ascii(result);
                if !dec_is_some {
                    // When it's None, the FheString returned is always empty
                    assert_eq!(dec_result, "");
                }

                dec_is_some.then_some(dec_result)
            })
            .collect();

        let dec_as_str: Vec<_> = dec
            .iter()
            .map(|option| option.as_ref().map(|s| s.as_str()))
            .collect();

        println!("\n\x1b[1mSplit_ascii_whitespace:\x1b[0m");
        result_message(str, &expected, &dec_as_str, end.duration_since(start));

        assert_eq!(dec_as_str, expected);
    }
}