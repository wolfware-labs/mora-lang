use std::sync::Arc;

use rustc_hash::FxHashMap;

/// An interned string.
///
/// Two symbols from the same [`Interner`] are equal exactly when their text
/// is equal. Ordering follows the order in which strings were first
/// interned, not their text, and symbols from different interners must not
/// be compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol(u32);

/// Stores each distinct string once and hands out [`Symbol`]s for them.
///
/// Symbols from one interner mean nothing to another, so one interner must
/// serve every string whose symbols are compared.
#[derive(Default, Debug)]
pub struct Interner {
    /// The symbol of each interned string.
    ids: FxHashMap<Arc<str>, Symbol>,
    /// The same allocations as the map keys, indexed by symbol.
    strings: Vec<Arc<str>>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the symbol for `text`, adding it on first use.
    ///
    /// # Panics
    ///
    /// Panics after `u32::MAX` distinct strings.
    pub fn intern(&mut self, text: &str) -> Symbol {
        if let Some(&symbol) = self.ids.get(text) {
            return symbol;
        }
        let Ok(raw) = u32::try_from(self.strings.len()) else {
            panic!("interner is full: more than {} distinct strings", u32::MAX);
        };
        let symbol = Symbol(raw);
        let text: Arc<str> = Arc::from(text);
        self.strings.push(Arc::clone(&text));
        self.ids.insert(text, symbol);
        symbol
    }

    /// Returns the symbol for `text` if it was interned before.
    pub fn get(&self, text: &str) -> Option<Symbol> {
        self.ids.get(text).copied()
    }

    /// The text of a symbol from this interner.
    ///
    /// # Panics
    ///
    /// Panics if the symbol came from another interner and is out of range.
    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_text_gives_same_symbol() {
        let mut interner = Interner::new();
        let a = interner.intern("Console");
        let b = interner.intern("WriteLine");
        assert_eq!(interner.intern("Console"), a);
        assert_ne!(a, b);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn resolve_returns_the_text() {
        let mut interner = Interner::new();
        let s = interner.intern("𝄞 clef");
        assert_eq!(interner.resolve(s), "𝄞 clef");
        assert_eq!(interner.get("𝄞 clef"), Some(s));
        assert_eq!(interner.get("missing"), None);
    }

    #[test]
    fn map_key_and_storage_share_one_allocation() {
        let mut interner = Interner::new();
        let s = interner.intern("shared");
        let (key, _) = interner.ids.get_key_value("shared").unwrap();
        assert!(Arc::ptr_eq(key, &interner.strings[s.0 as usize]));
    }

    #[test]
    fn is_empty_until_the_first_intern() {
        let mut interner = Interner::new();
        assert!(interner.is_empty());
        interner.intern("x");
        assert!(!interner.is_empty());
    }
}
