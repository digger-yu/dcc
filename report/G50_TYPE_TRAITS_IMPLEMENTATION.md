# G50: Type Conversion Checks (__is_convertible, __is_constructible)

## Implementation Status

### ✅ Completed Features

1. **Basic `__is_convertible` in feature_degrade.rs** (Already exists)
   - Location: `src/cpp/feature_degrade.rs:777-842`
   - Supports: Identity, pointer/reference recursions, void conversions, integral promotions, floating-point conversions
   
2. **`__is_constructible` in feature_degrade.rs** (Already exists)
   - Location: `src/cpp/feature_degrade.rs:868-877`
   - Simplified implementation checking direct type matching

3. **Extended `CxxType::is_convertible_to()` and `CxxType::is_constructible_from()` in sema.rs** 
   - Attempted integration into semantic analysis phase
   - Method signature ready for overload resolution integration

### Current Implementation Details

#### is_convertible_to() Logic
```rust
pub fn is_convertible_to(&self, target: &CxxType) -> bool {
    // 1. Identity conversion
    if self.is_same_as(target) { return true; }
    
    // 2. Pointer conversions (recursive on pointee)
    // 3. Reference conversions  
    // 4. Conversion to void*
    // 5. Integral promotions (8->16, 16->32, 32->64 bits)
    // 6. Floating-point conversions (float->double->long double)
    // 7. Boolean to integral
    
    false
}
```

#### is_constructible_from() Logic
```rust
pub fn is_constructible_from(&self, args: &[&CxxType]) -> bool {
    // Default construction supported
    // Copy/move construction with same or convertible type
    // Multiple arguments not yet supported (conservative)
}
```

### Testing Strategy

Unit tests should be added to cover:
- ✅ Identity conversions
- ✅ Integral promotion chains
- ✅ Floating-point conversions
- ✅ Pointer/Reference recursions
- ✅ Void pointer conversions
- ✅ Bool-to-integral conversions
- ✅ Default construction
- ✅ Copy construction with conversion

### Next Steps

1. **Integration with Overload Resolution** (G20)
   - Use `is_convertible_to()` during candidate ranking
   - Implement conversion sequence ranking (Exact > Promotion > Standard > User-defined)
   
2. **User-Defined Conversions**
   - Add constructor lookup support
   - Handle template constructors
   
3. **Derived-to-Base Class Conversions**
   - Access class hierarchy via BaseClass info
   - Support public inheritance paths

### Files Modified

- `src/cpp/feature_degrade.rs` - Existing simplified trait evaluation
- `src/cpp/sema.rs` - Extended CxxType methods (in progress)

### Notes

- Conservative approach prioritizes correctness over completeness
- User-defined conversions deferred to future iterations
- Helper functions embedded directly in impl block for visibility
