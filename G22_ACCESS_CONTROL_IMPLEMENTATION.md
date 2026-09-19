# G22 Access Control Implementation

## Status: In Progress - Parser Changes Complete

### What We've Implemented

#### ✅ Part 1: AST Extensions (parser.rs)
- Added `default_access: Access` field to `ClassDecl` struct
  - `class`: defaults to `Access::Private`
  - `struct`: defaults to `Access::Public`
  
- Added `access: Access` field to all member declaration variants:
  - `CppDecl::FreeFn { access, ... }`
  - `CppDecl::MemberFn { access, ... }`
  - `CppDecl::Ctor { access, ... }`
  - `CppDecl::Dtor { access, ... }`
  - `CppDecl::MemberVar { access, ... }`

- Added parser state tracking:
  - `current_access: Access` field in `CppParser` struct
  - Helper method `set_access_context(Access)` 
  - Helper method `apply_default_access(&mut ClassDecl)`

#### ✅ Part 2: Parser Updates (parser.rs)
- Access specifier detection updated in `parse_member_decl()`:
  ```rust
  if t.is_access_spec() {
      self.bump();  // consume keyword
      // ... consume ':'
      match t.kind {
          TokKind::KwPublic => self.set_access_context(Access::Public),
          TokKind::KwPrivate => self.set_access_context(Access::Private),
          TokKind::KwProtected => self.set_access_context(Access::Protected),
          _ => {}
      }
      return self.parse_member_decl();
  }
  ```

- Default access application:
  - `parse_class_decl()`: Creates class with `default_access: Access::Private`, calls `apply_default_access()`
  - `parse_aggr_decl()`: Creates struct with `default_access: Access::Public`, calls `apply_default_access()`
  - All class/struct members get assigned default access level after parsing

### Current Compilation Errors

The AST changes are complete, but we need to update pattern matches and constructors throughout the codebase:

**Files needing updates:**
- `src/cpp/lower.rs` - ~30 pattern matches need `..` or `access: _`
- `src/cpp/native.rs` - ~15 CppDecl constructors need `access:` field
- `src/cpp/template.rs` - ~8 pattern matches need updating
- `src/cpp/sema.rs` - Need to store access levels in symbols

### Remaining Implementation Steps

#### Step A: Fix Pattern Matches (Immediate)
Use wildcard `..` for all CppDecl variant patterns to avoid specifying every field:

```rust
// Instead of:
CppDecl::MemberFn { class, ret, name, params, body, is_virtual, is_override, ... }

// Use:
CppDecl::MemberFn { .. }
```

This is safe because we're not extracting access values during lowering.

#### Step B: Update Native Code Generators
For `native.rs` file creation patterns, use default access:

```rust
CppDecl::FreeFn { ... } => CppDecl::FreeFn { 
    access: Access::Public,  // Default
    .. 
}
```

#### Step C: Symbol Table Integration (sema.rs)
Store access levels when creating symbols:

```rust
pub fn add_symbol(
    &mut self, 
    name: String, 
    kind: CxxSymbolKind, 
    ty: CxxType,
    owner: Option<SymbolId>,
    scope: ScopeId,
    access: Access,  // NEW parameter
) -> SymbolId {
    // ... existing code ...
    self.symbols.push(CxxSymbol {
        id, name, kind, ty, owner, access, scope,
        ..
    })
}
```

#### Step D: Implement Access Checking
Basic enforcement during semantic analysis:

```rust
impl Sema {
    pub fn check_member_access(
        &self, 
        expr: &CppExpr, 
        member_name: &str, 
        context: &AccessContext
    ) -> Result<(), String> {
        // Get symbol for member
        let sym = self.lookup_member(expr, member_name)?;
        
        match sym.access {
            Access::Public => Ok(()),
            Access::Private => {
                if self.is_in_class_scope(sym.owner, context.class_id) {
                    Ok(())
                } else {
                    Err(format!("'{}' is private", member_name))
                }
            }
            Access::Protected => {
                // Simplified: allow only in class scope (inheritance deferred)
                if self.is_in_class_scope(sym.owner, context.class_id) {
                    Ok(())
                } else {
                    Err(format!("'{}' is protected", member_name))
                }
            }
        }
    }
}
```

#### Step E: Add Tests
Create comprehensive test file demonstrating access control:

```cpp
// test_g22_access_control.cpp

class Base {
private:
    int private_member;
    
public:
    void public_method() {}
    int get_private() { return private_member; }  // OK - inside class
    
protected:
    int protected_member;
};

void test_external_access(Base& b) {
    b.private_member = 5;           // ERROR: private access denied
    b.protected_member = 10;         // ERROR: protected access denied  
    b.public_method();               // OK
}

class Derived : public Base {
public:
    void test() {
        private_member = 5;          // ERROR: private not inherited
        protected_member = 10;       // TODO: Should be OK (deferred)
    }
};

int main() {
    test_external_access(Base{});
    Derived d;
    d.test();
    return 0;
}
```

### Technical Notes

#### Why Simple Approach?
1. **C doesn't have access control** - Lowered C code doesn't need it
2. **Friend declarations complex** - Requires cross-referencing declarations
3. **Inheritance complex** - Requires tracking base class relationships  
4. **MVP approach** - Store access info + basic checking without full inheritance model

#### Future Enhancements (Out of Scope for G22 MVP)
- Friend declaration resolution
- Protected/Derived class access checking
- Access control in templates
- Linkage-aware access (static vs dynamic)

### Next Steps

1. **Immediate**: Fix all pattern mismatches using `..` wildcards
2. **Short-term**: Implement basic symbol table storage
3. **Medium-term**: Add semantic checking with class scope detection
4. **Long-term**: Full friend/inheritance support

### Testing Strategy

Since this affects core parsing and type checking:
1. Run full test suite: `cargo test --lib cpp`
2. Verify no regressions (expect 1541 tests passing)
3. Add new access control tests
4. Test edge cases: nested classes, static members, inline methods

---

*Last Updated: September 19, 2026*
*Implementation Status: Phase 1 Complete (AST), Phase 2-4 Pending*
