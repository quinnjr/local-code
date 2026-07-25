//! Shared URL helpers.

/// Percent-encodes one URL path segment per RFC 3986 — only the unreserved
/// set (`A-Z a-z 0-9 - _ . ~`) passes through, everything else becomes
/// `%XX` of its UTF-8 bytes. Used when building listing links, redirect
/// targets, the artifact tool's file URLs, and GitLab API paths, so names
/// with spaces or non-ASCII still produce valid, clickable URLs.
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_characters_pass_through_untouched() {
        assert_eq!(encode_path_segment("abcXYZ019-_.~"), "abcXYZ019-_.~");
    }

    #[test]
    fn reserved_characters_are_percent_encoded() {
        assert_eq!(encode_path_segment("my file"), "my%20file");
        assert_eq!(encode_path_segment("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
        assert_eq!(encode_path_segment("100%"), "100%25");
    }

    #[test]
    fn non_ascii_is_encoded_per_utf8_byte() {
        assert_eq!(encode_path_segment("ü"), "%C3%BC");
    }

    #[test]
    fn an_empty_segment_encodes_to_empty() {
        assert_eq!(encode_path_segment(""), "");
    }
}
