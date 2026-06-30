//! GUI view layer: the toolbar, panels, EQ curve (with the spectrum drawn behind
//! it), effects/bands tables, device/profile sections, dialogs, and shared widget
//! helpers. Each submodule hangs `impl GuiApp` methods (or free helper fns) off
//! the type defined in [`crate::app`].

pub(crate) mod apps;
pub(crate) mod bands;
pub(crate) mod curve_view;
pub(crate) mod devices;
pub(crate) mod dialogs;
pub(crate) mod effects;
pub(crate) mod icons;
pub(crate) mod kit;
pub(crate) mod layout;
pub(crate) mod reference_bar;
pub(crate) mod toolbar;
pub(crate) mod widgets;
