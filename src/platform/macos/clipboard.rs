//! General pasteboard access.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::error::{Error, ErrorCode, Result};

/// Marks content that clipboard managers should not record.
/// See <http://nspasteboard.org>.
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// Saved pasteboard items, each a list of (type, data) pairs.
pub struct Saved(Vec<Vec<(String, Vec<u8>)>>);

/// Copies every item and type currently on the pasteboard.
pub fn save() -> Saved {
    let items = NSPasteboard::generalPasteboard()
        .pasteboardItems()
        .map(|items| items.to_vec())
        .unwrap_or_default();
    let saved = items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| {
                    item.dataForType(&ty)
                        .map(|data| (ty.to_string(), data.to_vec()))
                })
                .collect()
        })
        .collect();
    Saved(saved)
}

/// Replaces the pasteboard with plain text and optional HTML.
pub fn set(plain: &str, html: Option<&str>) -> Result<()> {
    let item = NSPasteboardItem::new();
    item.setString_forType(
        &NSString::from_str(plain),
        &NSString::from_str("public.utf8-plain-text"),
    );
    if let Some(html) = html {
        item.setString_forType(
            &NSString::from_str(html),
            &NSString::from_str("public.html"),
        );
    }
    item.setData_forType(&NSData::new(), &NSString::from_str(TRANSIENT_TYPE));
    write(vec![item])
}

/// Puts saved items back on the pasteboard.
pub fn restore(saved: Saved) -> Result<()> {
    let items = saved
        .0
        .into_iter()
        .map(|types| {
            let item = NSPasteboardItem::new();
            for (ty, data) in types {
                item.setData_forType(&NSData::with_bytes(&data), &NSString::from_str(&ty));
            }
            item
        })
        .collect();
    write(items)
}

fn write(items: Vec<Retained<NSPasteboardItem>>) -> Result<()> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if items.is_empty() {
        return Ok(());
    }
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
        .into_iter()
        .map(ProtocolObject::from_retained)
        .collect();
    if !pasteboard.writeObjects(&NSArray::from_retained_slice(&objects)) {
        return Err(Error::new(
            ErrorCode::Platform,
            "writing to the clipboard failed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "replaces and then restores the real clipboard"]
    fn set_and_restore_round_trip() {
        let original = save().0;

        set("Troy Barnes", Some("<b>Troy Barnes</b>")).expect("set");
        let current = save().0;
        let types: Vec<&str> = current[0].iter().map(|(ty, _)| ty.as_str()).collect();
        assert!(types.contains(&"public.utf8-plain-text"), "{types:?}");
        assert!(types.contains(&"public.html"), "{types:?}");
        assert!(types.contains(&TRANSIENT_TYPE), "{types:?}");
        let plain = current[0]
            .iter()
            .find(|(ty, _)| ty == "public.utf8-plain-text")
            .map(|(_, data)| String::from_utf8_lossy(data).into_owned());
        assert_eq!(plain.as_deref(), Some("Troy Barnes"));

        restore(Saved(original.clone())).expect("restore");
        assert_eq!(save().0, original);
    }
}
