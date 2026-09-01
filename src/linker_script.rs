//! GNU linker script parser for bare-metal embedded targets.
//!
//! This module provides parsing for GNU LD linker scripts used
//! in embedded systems to define memory layout, section placement,
//! and symbol definitions.

use std::collections::HashMap;

/// Memory region definition from MEMORY command
#[derive(Debug, Clone)]
pub struct MemoryRegion {
    pub name: String,
    pub origin: u64,
    pub length: u64,
    pub attributes: String, // e.g., "rx", "rwx"
}

/// Section placement in linker script
#[derive(Debug, Clone)]
pub struct SectionPlacement {
    pub output_section: String,
    pub input_sections: Vec<String>,
    pub memory_region: Option<String>,
}

/// Parsed linker script
#[derive(Debug, Clone, Default)]
pub struct LinkerScript {
    pub memory_regions: Vec<MemoryRegion>,
    pub section_placements: Vec<SectionPlacement>,
    pub entry_symbol: Option<String>,
    pub symbols: HashMap<String, u64>,
}

/// Parse a GNU linker script.
///
/// Supports basic MEMORY and SECTIONS commands.
pub fn parse_linker_script(content: &str) -> Result<LinkerScript, String> {
    let mut script = LinkerScript::default();
    let mut chars = content.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            '/' if content.contains("/*") => {
                // Skip comments
                skip_comment(&mut chars, content);
            }
            'M' => {
                if try_parse_keyword(&mut chars, "MEMORY") {
                    parse_memory_block(&mut chars, content, &mut script)?;
                } else {
                    chars.next();
                }
            }
            'S' => {
                if try_parse_keyword(&mut chars, "SECTIONS") {
                    parse_sections_block(&mut chars, content, &mut script)?;
                } else {
                    chars.next();
                }
            }
            'E' | 'e' => {
                if try_parse_keyword(&mut chars, "ENTRY") {
                    parse_entry(&mut chars, content, &mut script)?;
                } else {
                    chars.next();
                }
            }
            _ => {
                chars.next();
            }
        }
    }

    Ok(script)
}

fn skip_comment(chars: &mut std::iter::Peekable<std::str::Chars>, _content: &str) {
    chars.next(); // /
    if chars.peek() == Some(&'*') {
        chars.next(); // *
        while let Some(ch) = chars.next() {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                break;
            }
        }
    }
}

fn try_parse_keyword(chars: &mut std::iter::Peekable<std::str::Chars>, keyword: &str) -> bool {
    let saved: Vec<char> = chars.clone().take(keyword.len()).collect();
    let found: String = saved.iter().collect();
    if found == keyword {
        for _ in 0..keyword.len() {
            chars.next();
        }
        true
    } else {
        false
    }
}

fn parse_memory_block(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    _content: &str,
    script: &mut LinkerScript,
) -> Result<(), String> {
    skip_whitespace(chars);
    if chars.peek() != Some(&'{') {
        return Err("expected '{' after MEMORY".to_string());
    }
    chars.next(); // {

    loop {
        skip_whitespace(chars);
        if chars.peek() == Some(&'}') {
            chars.next();
            break;
        }

        // Parse region name
        let name = parse_identifier(chars);
        skip_whitespace(chars);

        // Parse attributes (optional)
        let mut attributes = String::new();
        if chars.peek() == Some(&'(') {
            chars.next();
            while let Some(&ch) = chars.peek() {
                if ch == ')' {
                    chars.next();
                    break;
                }
                attributes.push(ch);
                chars.next();
            }
        }
        skip_whitespace(chars);

        // Parse : origin = 0x... length = 0x...
        if chars.peek() == Some(&':') {
            chars.next();
        }
        skip_whitespace(chars);

        let mut origin = 0u64;
        let mut length = 0u64;

        // Parse ORIGIN and LENGTH
        while chars.peek() != Some(&'}') && chars.peek().is_some() {
            skip_whitespace(chars);
            let ident = parse_identifier(chars);
            skip_whitespace(chars);

            if ident == "ORIGIN" || ident == "o" || ident == "org" {
                if chars.peek() == Some(&'=') {
                    chars.next();
                }
                skip_whitespace(chars);
                origin = parse_number(chars);
            } else if ident == "LENGTH" || ident == "l" || ident == "len" {
                if chars.peek() == Some(&'=') {
                    chars.next();
                }
                skip_whitespace(chars);
                length = parse_number(chars);
            }

            if chars.peek() == Some(&',') {
                chars.next();
            }
            if chars.peek() == Some(&'\n') || chars.peek() == Some(&'}') {
                break;
            }
        }

        if !name.is_empty() {
            script.memory_regions.push(MemoryRegion {
                name,
                origin,
                length,
                attributes,
            });
        }
    }

    Ok(())
}

fn parse_sections_block(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    _content: &str,
    script: &mut LinkerScript,
) -> Result<(), String> {
    skip_whitespace(chars);
    if chars.peek() != Some(&'{') {
        return Err("expected '{' after SECTIONS".to_string());
    }
    chars.next(); // {

    let mut depth = 1;
    while depth > 0 {
        skip_whitespace(chars);
        match chars.peek() {
            Some(&'}') => {
                chars.next();
                depth -= 1;
            }
            Some(&'{') => {
                chars.next();
                depth += 1;
            }
            Some(_) => {
                chars.next();
            }
            None => break,
        }
    }

    Ok(())
}

fn parse_entry(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    _content: &str,
    script: &mut LinkerScript,
) -> Result<(), String> {
    skip_whitespace(chars);
    if chars.peek() == Some(&'(') {
        chars.next();
        skip_whitespace(chars);
        let symbol = parse_identifier(chars);
        script.entry_symbol = Some(symbol);
        // Skip to closing paren
        while let Some(&ch) = chars.peek() {
            chars.next();
            if ch == ')' {
                break;
            }
        }
    }
    Ok(())
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

fn parse_identifier(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut ident = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_alphanumeric() || ch == '_' {
            ident.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    ident
}

fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars>) -> u64 {
    let mut num_str = String::new();
    let mut is_hex = false;

    // Check for 0x prefix
    if chars.peek() == Some(&'0') {
        num_str.push('0');
        chars.next();
        if chars.peek() == Some(&'x') || chars.peek() == Some(&'X') {
            is_hex = true;
            num_str.push('x');
            chars.next();
        }
    }

    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_hexdigit() {
            num_str.push(ch);
            chars.next();
        } else {
            break;
        }
    }

    let base_value = if is_hex {
        u64::from_str_radix(&num_str[2..], 16).unwrap_or(0)
    } else {
        num_str.parse().unwrap_or(0)
    };

    // Handle K/M suffixes
    let multiplier = match chars.peek() {
        Some(&'K') | Some(&'k') => {
            chars.next();
            1024u64
        }
        Some(&'M') | Some(&'m') => {
            chars.next();
            1024 * 1024u64
        }
        _ => 1u64,
    };

    base_value * multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_memory() {
        let script = r#"
            MEMORY {
                FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 256K
                RAM (rwx)   : ORIGIN = 0x20000000, LENGTH = 64K
            }
        "#;
        let result = parse_linker_script(script).unwrap();
        assert_eq!(result.memory_regions.len(), 2);
        assert_eq!(result.memory_regions[0].name, "FLASH");
        assert_eq!(result.memory_regions[0].origin, 0x08000000);
        assert_eq!(result.memory_regions[0].length, 256 * 1024);
        assert_eq!(result.memory_regions[1].name, "RAM");
        assert_eq!(result.memory_regions[1].length, 64 * 1024);
    }

    #[test]
    fn parse_entry_symbol() {
        let script = "ENTRY(Reset_Handler)";
        let result = parse_linker_script(script).unwrap();
        assert_eq!(result.entry_symbol.as_deref(), Some("Reset_Handler"));
    }
}
