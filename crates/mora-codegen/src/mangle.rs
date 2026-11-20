//! Symbol names.

use mora_hir::{BodyOwner, Ty, owner_name};
use mora_span::Db;

/// The symbol of the function whose qualified name has the given segments:
/// `_M`, then each segment as its byte length in decimal followed by the
/// segment, then `E`.
pub fn mangle<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
    let mut symbol = String::from("_M");
    for segment in segments {
        symbol.push_str(&segment.len().to_string());
        symbol.push_str(segment);
    }
    symbol.push('E');
    symbol
}

/// The segment of a declared function: its name followed by the types of
/// its parameters, between parentheses and separated by commas, as in
/// `Add(int,string)`, so that each overload has a symbol of its own.
pub(crate) fn parameter_segment<'a>(
    name: &str,
    params: impl IntoIterator<Item = &'a Ty>,
    db: &dyn Db,
) -> String {
    let params: Vec<String> = params.into_iter().map(|ty| type_text(db, ty)).collect();
    format!("{name}({})", params.join(","))
}

/// A type as a function's segment writes it: a primitive or `string` by its
/// keyword, a struct by its full name, and a tuple as its element types between
/// parentheses, separated by commas and with a comma after a single one,
/// without the elements' names, which do not tell overloads apart (§5.5).
fn type_text(db: &dyn Db, ty: &Ty) -> String {
    match ty {
        Ty::Tuple(elems) => {
            let mut elems: Vec<String> = elems.iter().map(|elem| type_text(db, &elem.ty)).collect();
            if let [only] = elems.as_mut_slice() {
                only.push(',');
            }
            format!("({})", elems.join(","))
        }
        Ty::Struct(item) => owner_name(db, item.project, item.file, BodyOwner::Item(item.item)),
        _ => mora_hir::ty_name(db, ty),
    }
}

#[cfg(test)]
mod tests {
    use super::mangle;

    #[test]
    fn mangle_prefixes_each_segment_with_its_byte_length() {
        assert_eq!(
            mangle(["UsersApi", "UserStore", "Add"]),
            "_M8UsersApi9UserStore3AddE"
        );
        assert_eq!(mangle(["Ünï"]), "_M5ÜnïE");
    }
}
