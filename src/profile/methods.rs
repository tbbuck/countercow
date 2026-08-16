//! The method table: turning a stack frame's instruction pointer into a name.

use crate::nettrace::reader::Reader;

/// One jitted method's address range and name.
#[derive(Debug, Clone)]
pub struct Method {
    pub start_address: u64,
    pub size: u32,
    pub namespace: String,
    pub name: String,
    pub signature: String,
}

impl Method {
    /// `Namespace.Method`, or just the method when it has no namespace.
    pub fn qualified_name(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.namespace, self.name)
        }
    }

    pub fn contains(&self, address: u64) -> bool {
        address >= self.start_address && address < self.start_address + u64::from(self.size)
    }
}

/// MethodLoadVerbose / MethodDCEndVerbose:
/// `MethodID:u64, ModuleID:u64, MethodStartAddress:u64, MethodSize:u32, MethodToken:u32,
/// MethodFlags:u32, MethodNamespace:wstr, MethodName:wstr, MethodSignature:wstr, ClrInstanceID:u16`
/// with a trailing `ReJITID:u64` in later versions, which we do not need.
pub fn decode_method(payload: &[u8]) -> Option<Method> {
    let mut r = Reader::new(payload);

    r.skip(8, "MethodID").ok()?;
    r.skip(8, "ModuleID").ok()?;
    let start_address = r.u64().ok()?;
    let size = r.u32().ok()?;
    r.skip(4, "MethodToken").ok()?;
    r.skip(4, "MethodFlags").ok()?;

    Some(Method {
        start_address,
        size,
        namespace: r.utf16_nul_string().ok()?,
        name: r.utf16_nul_string().ok()?,
        signature: r.utf16_nul_string().ok()?,
    })
}

/// Address-to-method lookup over the whole process.
///
/// Built once when the rundown lands, then queried per sampled address, so it is sorted for
/// binary search rather than hashed: addresses are ranges, not keys.
#[derive(Debug, Default)]
pub struct MethodTable {
    methods: Vec<Method>,
    sorted: bool,
}

impl MethodTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, method: Method) {
        // A zero-length method can never match an address, and would break range containment.
        if method.size == 0 {
            return;
        }
        self.methods.push(method);
        self.sorted = false;
    }

    pub fn len(&self) -> usize {
        self.methods.len()
    }

    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }

    /// Sort for lookup. Cheap to call repeatedly; only re-sorts after an insert.
    pub fn finish(&mut self) {
        if !self.sorted {
            self.methods.sort_by_key(|m| m.start_address);
            self.sorted = true;
        }
    }

    /// The method containing `address`, if any.
    ///
    /// Returns `None` for native frames — the runtime itself, the OS, anything not jitted — which
    /// are a normal and large fraction of any real stack.
    pub fn resolve(&self, address: u64) -> Option<&Method> {
        debug_assert!(self.sorted, "call finish() before resolving");

        // The last method starting at or below the address is the only candidate.
        let index = self.methods.partition_point(|m| m.start_address <= address);
        let candidate = self.methods.get(index.checked_sub(1)?)?;
        candidate.contains(address).then_some(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(start: u64, size: u32, name: &str) -> Method {
        Method {
            start_address: start,
            size,
            namespace: "Ns".into(),
            name: name.into(),
            signature: "()".into(),
        }
    }

    fn table(methods: Vec<Method>) -> MethodTable {
        let mut table = MethodTable::new();
        for m in methods {
            table.insert(m);
        }
        table.finish();
        table
    }

    #[test]
    fn resolves_an_address_inside_a_method() {
        let table = table(vec![method(1000, 100, "A"), method(2000, 100, "B")]);
        assert_eq!(table.resolve(1000).unwrap().name, "A");
        assert_eq!(table.resolve(1050).unwrap().name, "A");
        assert_eq!(table.resolve(2099).unwrap().name, "B");
    }

    #[test]
    fn addresses_outside_every_method_resolve_to_nothing() {
        // Native frames are normal; guessing at them would be worse than admitting ignorance.
        let table = table(vec![method(1000, 100, "A"), method(2000, 100, "B")]);
        assert!(table.resolve(999).is_none(), "before the first method");
        assert!(table.resolve(1100).is_none(), "in the gap between methods");
        assert!(table.resolve(5000).is_none(), "past the last method");
    }

    #[test]
    fn boundaries_are_half_open() {
        let table = table(vec![method(1000, 100, "A")]);
        assert!(table.resolve(1000).is_some(), "start is inclusive");
        assert!(table.resolve(1099).is_some());
        assert!(table.resolve(1100).is_none(), "start + size is exclusive");
    }

    #[test]
    fn insertion_order_does_not_matter() {
        let table = table(vec![method(3000, 10, "C"), method(1000, 10, "A"), method(2000, 10, "B")]);
        assert_eq!(table.resolve(1005).unwrap().name, "A");
        assert_eq!(table.resolve(2005).unwrap().name, "B");
        assert_eq!(table.resolve(3005).unwrap().name, "C");
    }

    #[test]
    fn zero_length_methods_are_rejected() {
        let mut table = MethodTable::new();
        table.insert(method(1000, 0, "Empty"));
        table.finish();
        assert!(table.is_empty());
        assert!(table.resolve(1000).is_none());
    }

    #[test]
    fn qualified_name_handles_a_missing_namespace() {
        let mut global = method(0, 1, "Main");
        global.namespace = String::new();
        assert_eq!(global.qualified_name(), "Main");
        assert_eq!(method(0, 1, "Run").qualified_name(), "Ns.Run");
    }

    #[test]
    fn an_empty_table_resolves_nothing_without_panicking() {
        let table = table(vec![]);
        assert!(table.resolve(1234).is_none());
    }
}
