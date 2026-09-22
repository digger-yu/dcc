// Template string substitution for inline assembly
// Handles: %N placeholders, modifiers, labels

use std::collections::HashMap;

/// Replace %N placeholders with actual values
pub fn substitute_template(template: &str, operands: &HashMap<usize, String>) -> String {
    let mut result = template.to_string();
    
    // Replace %0, %1, ... %9
    for i in 0..=9 {
        let placeholder = format!("%{}", i);
        if let Some(value) = operands.get(&i) {
            result = result.replace(&placeholder, value);
        }
    }
    
    // Replace %[name] style placeholders
    for (key, value) in operands {
        if *key > 9 {
            let placeholder = format!("%[{}]", key);
            result = result.replace(&placeholder, value);
        }
    }
    
    result
}
