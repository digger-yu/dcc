// src/parse_profile.rs - Parse profiling data
//
// Collects timing and counter data for the preprocessing, lexing, and parsing
// phases. Output as machine-readable JSON when --profile-parse is set.

#[derive(Debug, Clone, Default)]
pub struct ParseProfile {
    pub preprocess_time_ns: u64,
    pub lex_time_ns: u64,
    pub parse_time_ns: u64,
    pub token_count: u64,
    pub ast_node_count: u64,
    pub alloc_count: u64,
    pub alloc_bytes: u64,
    pub include_count: u64,
    pub macro_expand_count: u64,
    pub diagnostic_count: u64,
}

impl ParseProfile {
    pub fn to_json(&self) -> String {
        format!(
            r#"{{
  "preprocess_time_ns": {},
  "lex_time_ns": {},
  "parse_time_ns": {},
  "token_count": {},
  "ast_node_count": {},
  "alloc_count": {},
  "alloc_bytes": {},
  "include_count": {},
  "macro_expand_count": {},
  "diagnostic_count": {}
}}"#,
            self.preprocess_time_ns,
            self.lex_time_ns,
            self.parse_time_ns,
            self.token_count,
            self.ast_node_count,
            self.alloc_count,
            self.alloc_bytes,
            self.include_count,
            self.macro_expand_count,
            self.diagnostic_count,
        )
    }
}
