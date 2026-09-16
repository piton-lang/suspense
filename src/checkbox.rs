//! A checkbox with its label beside it, legible in full. gpui-kit's own label
//! sits in a box clipped to a single line of exactly its font size, which cuts
//! off the bottoms of letters like p and y, and of brackets; this label is a
//! child with room for them instead.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::*;

/// How tall a line of the label is, against its font size: enough for
/// descenders.
const LABEL_LINE_HEIGHT: f32 = 1.4;

/// A checkbox `id`, labelled `label`, vertically centred on the label.
pub fn checkbox(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Checkbox {
    let id = id.into();
    let label = div()
        .id(ElementId::Name(format!("{id}-label").into()))
        .line_height(relative(LABEL_LINE_HEIGHT))
        .child(label.into());
    // Lets UI tests find the label; inert in normal builds.
    Checkbox::new(id)
        .items_center()
        .child(gpui_kit::TestSupportExt::test_support(label))
}
