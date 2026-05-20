#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct UiId(pub(crate) u64);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UiRect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl UiRect {
    pub(crate) fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.width && y <= self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UiSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayoutConstraints {
    pub(crate) min: UiSize,
    pub(crate) max: UiSize,
}

impl LayoutConstraints {
    pub(crate) fn tight(size: UiSize) -> Self {
        Self { min: size, max: size }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UiNodeKind {
    Root,
    Row,
    Column,
    Stack,
    SplitPane,
    ScrollContainer,
    VirtualList,
    Surface,
    Text,
    Image,
    Overlay,
    SemanticOnly,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirtyFlags {
    pub(crate) layout: bool,
    pub(crate) paint: bool,
    pub(crate) text: bool,
    pub(crate) semantics: bool,
}

impl DirtyFlags {
    pub(crate) fn any(&self) -> bool {
        self.layout || self.paint || self.text || self.semantics
    }

    pub(crate) fn mark_all(&mut self) {
        self.layout = true;
        self.paint = true;
        self.text = true;
        self.semantics = true;
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiNode {
    pub(crate) id: UiId,
    pub(crate) kind: UiNodeKind,
    pub(crate) bounds: UiRect,
    pub(crate) children: Vec<UiId>,
    pub(crate) dirty: DirtyFlags,
    pub(crate) semantic_role: Option<AccessibilityRole>,
    pub(crate) label: Option<String>,
    pub(crate) cache_key: Option<u64>,
}

impl UiNode {
    pub(crate) fn new(id: UiId, kind: UiNodeKind) -> Self {
        Self {
            id,
            kind,
            bounds: UiRect::default(),
            children: Vec::new(),
            dirty: DirtyFlags::default(),
            semantic_role: None,
            label: None,
            cache_key: None,
        }
    }

    pub(crate) fn with_semantics(
        mut self,
        role: AccessibilityRole,
        label: impl Into<String>,
    ) -> Self {
        self.semantic_role = Some(role);
        self.label = Some(label.into());
        self
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RetainedUiTree {
    pub(crate) root: Option<UiId>,
    pub(crate) nodes: BTreeMap<UiId, UiNode>,
    dirty_nodes: BTreeSet<UiId>,
}

impl RetainedUiTree {
    pub(crate) fn upsert(&mut self, mut node: UiNode) {
        let existing_key = self.nodes.get(&node.id).and_then(|existing| existing.cache_key);
        if existing_key != node.cache_key {
            node.dirty.mark_all();
        }
        if node.dirty.any() {
            self.dirty_nodes.insert(node.id);
        }
        self.nodes.insert(node.id, node);
    }

    pub(crate) fn mark_dirty(&mut self, id: UiId, flags: DirtyFlags) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.dirty.layout |= flags.layout;
            node.dirty.paint |= flags.paint;
            node.dirty.text |= flags.text;
            node.dirty.semantics |= flags.semantics;
            self.dirty_nodes.insert(id);
        }
    }

    pub(crate) fn dirty_nodes(&self) -> impl Iterator<Item = &UiNode> {
        self.dirty_nodes
            .iter()
            .filter_map(|id| self.nodes.get(id))
    }

    pub(crate) fn clear_dirty(&mut self) {
        for id in std::mem::take(&mut self.dirty_nodes) {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.dirty.clear();
            }
        }
    }

    pub(crate) fn semantics(&self) -> Vec<SemanticNode> {
        self.nodes
            .values()
            .filter_map(|node| {
                Some(SemanticNode {
                    id: node.id,
                    role: node.semantic_role?,
                    label: node.label.clone().unwrap_or_default(),
                    bounds: node.bounds,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DisplayList {
    pub(crate) commands: Vec<DisplayCommand>,
    pub(crate) semantic_nodes: Vec<SemanticNode>,
}

impl DisplayList {
    pub(crate) fn push(&mut self, command: DisplayCommand) {
        self.commands.push(command);
    }

    pub(crate) fn extend_semantics(&mut self, nodes: impl IntoIterator<Item = SemanticNode>) {
        self.semantic_nodes.extend(nodes);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DisplayCommand {
    Rect {
        id: UiId,
        rect: UiRect,
        color: ColorRgba,
    },
    RoundedRect {
        id: UiId,
        rect: UiRect,
        radius: f32,
        color: ColorRgba,
    },
    Border {
        id: UiId,
        rect: UiRect,
        width: f32,
        color: ColorRgba,
    },
    Text {
        id: UiId,
        origin: (f32, f32),
        runs: Vec<DisplayTextRun>,
    },
    Image {
        id: UiId,
        rect: UiRect,
        image: DisplayImageRef,
    },
    ClipStart {
        id: UiId,
        rect: UiRect,
    },
    ClipEnd {
        id: UiId,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ColorRgba {
    pub(crate) r: f32,
    pub(crate) g: f32,
    pub(crate) b: f32,
    pub(crate) a: f32,
}

impl ColorRgba {
    pub(crate) const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DisplayTextRun {
    pub(crate) text: String,
    pub(crate) font_stack: FontFallbackStack,
    pub(crate) size_px: f32,
    pub(crate) color: ColorRgba,
    pub(crate) attrs: TextAttributes,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FontFallbackStack {
    pub(crate) primary: String,
    pub(crate) fallbacks: Vec<String>,
}

impl FontFallbackStack {
    pub(crate) fn new(primary: impl Into<String>, fallbacks: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            primary: primary.into(),
            fallbacks: fallbacks.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct TextAttributes {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) monospace: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DisplayImageRef {
    TextureId(String),
    AttachmentId(String),
    PendingDecode(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TextShapingMode {
    BasicAscii,
    UnicodeShaping,
    PlatformNative,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TextEngineConfig {
    pub(crate) shaping: TextShapingMode,
    pub(crate) font_stack: FontFallbackStack,
    pub(crate) enable_ligatures: bool,
    pub(crate) enable_emoji_fallback: bool,
}

impl TextEngineConfig {
    pub(crate) fn desktop_default() -> Self {
        Self {
            shaping: TextShapingMode::UnicodeShaping,
            font_stack: FontFallbackStack::new(
                "JetBrainsMono Nerd Font",
                ["JetBrainsMono Nerd Font Mono", "JetBrains Mono", "monospace"],
            ),
            enable_ligatures: false,
            enable_emoji_fallback: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct GlyphAtlasLifecycle {
    pub(crate) font_epoch: u64,
    pub(crate) atlas_generation: u64,
    pub(crate) glyph_count: usize,
    pub(crate) byte_estimate: usize,
    pub(crate) evictions: u64,
}

impl GlyphAtlasLifecycle {
    pub(crate) fn note_font_stack_changed(&mut self) {
        self.font_epoch += 1;
        self.atlas_generation += 1;
        self.glyph_count = 0;
        self.byte_estimate = 0;
    }

    pub(crate) fn note_glyphs_uploaded(&mut self, glyph_count: usize, bytes: usize) {
        self.glyph_count = self.glyph_count.saturating_add(glyph_count);
        self.byte_estimate = self.byte_estimate.saturating_add(bytes);
    }

    pub(crate) fn evict_all(&mut self) {
        self.atlas_generation += 1;
        self.glyph_count = 0;
        self.byte_estimate = 0;
        self.evictions += 1;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImeState {
    pub(crate) active: bool,
    pub(crate) preedit: String,
    pub(crate) cursor_byte_range: Option<(usize, usize)>,
}

impl ImeState {
    pub(crate) fn apply_preedit(&mut self, text: impl Into<String>, cursor: Option<(usize, usize)>) {
        self.active = true;
        self.preedit = text.into();
        self.cursor_byte_range = cursor;
    }

    pub(crate) fn commit(&mut self) -> String {
        self.active = false;
        self.cursor_byte_range = None;
        std::mem::take(&mut self.preedit)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum AccessibilityRole {
    Window,
    Workspace,
    Surface,
    Transcript,
    Message,
    Button,
    TextInput,
    StaticText,
    Image,
    Code,
    ToolCard,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticNode {
    pub(crate) id: UiId,
    pub(crate) role: AccessibilityRole,
    pub(crate) label: String,
    pub(crate) bounds: UiRect,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesktopTheme {
    pub(crate) mode: ThemeMode,
    pub(crate) background: ColorRgba,
    pub(crate) panel: ColorRgba,
    pub(crate) text: ColorRgba,
    pub(crate) muted_text: ColorRgba,
    pub(crate) accent: ColorRgba,
    pub(crate) error: ColorRgba,
}

impl DesktopTheme {
    pub(crate) fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            background: ColorRgba::rgba(0.965, 0.972, 0.985, 1.0),
            panel: ColorRgba::rgba(1.0, 1.0, 1.0, 0.82),
            text: ColorRgba::rgba(0.12, 0.13, 0.16, 1.0),
            muted_text: ColorRgba::rgba(0.38, 0.40, 0.45, 1.0),
            accent: ColorRgba::rgba(0.30, 0.42, 0.95, 1.0),
            error: ColorRgba::rgba(0.85, 0.12, 0.16, 1.0),
        }
    }

    pub(crate) fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            background: ColorRgba::rgba(0.055, 0.060, 0.075, 1.0),
            panel: ColorRgba::rgba(0.11, 0.12, 0.15, 0.86),
            text: ColorRgba::rgba(0.88, 0.90, 0.94, 1.0),
            muted_text: ColorRgba::rgba(0.60, 0.63, 0.70, 1.0),
            accent: ColorRgba::rgba(0.50, 0.62, 1.0, 1.0),
            error: ColorRgba::rgba(1.0, 0.38, 0.42, 1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiPreferences {
    pub(crate) theme_mode: ThemeMode,
    pub(crate) font_scale: f32,
    pub(crate) reduced_motion: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::System,
            font_scale: 1.0,
            reduced_motion: false,
        }
    }
}

impl UiPreferences {
    pub(crate) fn clamped_font_scale(&self) -> f32 {
        self.font_scale.clamp(0.65, 1.60)
    }

    pub(crate) fn animation_duration_ms(&self, default_ms: u64) -> u64 {
        if self.reduced_motion { 0 } else { default_ms }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VirtualListState {
    pub(crate) total_items: usize,
    pub(crate) first_visible: usize,
    pub(crate) visible_count: usize,
    pub(crate) overscan: usize,
}

impl VirtualListState {
    pub(crate) fn materialized_range(self) -> std::ops::Range<usize> {
        let start = self.first_visible.saturating_sub(self.overscan).min(self.total_items);
        let visible_end = self
            .first_visible
            .saturating_add(self.visible_count)
            .min(self.total_items);
        let end = visible_end.saturating_add(self.overscan).min(self.total_items);
        start..end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SurfaceRenderCache {
    pub(crate) surface_id: UiId,
    pub(crate) layout_key: u64,
    pub(crate) display_key: u64,
    pub(crate) semantic_key: u64,
    pub(crate) invalidation_epoch: u64,
}

impl SurfaceRenderCache {
    pub(crate) fn new(surface_id: UiId) -> Self {
        Self {
            surface_id,
            layout_key: 0,
            display_key: 0,
            semantic_key: 0,
            invalidation_epoch: 0,
        }
    }

    pub(crate) fn update_keys(&mut self, layout_key: u64, display_key: u64, semantic_key: u64) -> DirtyFlags {
        let mut flags = DirtyFlags::default();
        if self.layout_key != layout_key {
            self.layout_key = layout_key;
            flags.layout = true;
        }
        if self.display_key != display_key {
            self.display_key = display_key;
            flags.paint = true;
        }
        if self.semantic_key != semantic_key {
            self.semantic_key = semantic_key;
            flags.semantics = true;
        }
        if flags.any() {
            self.invalidation_epoch += 1;
        }
        flags
    }
}

pub(crate) fn stable_hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_tree_tracks_dirty_nodes_and_semantics() {
        let mut tree = RetainedUiTree::default();
        let mut node = UiNode::new(UiId(1), UiNodeKind::Text)
            .with_semantics(AccessibilityRole::StaticText, "hello");
        node.bounds = UiRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        node.cache_key = Some(10);
        tree.upsert(node.clone());
        assert_eq!(tree.dirty_nodes().count(), 1);
        let semantics = tree.semantics();
        assert_eq!(semantics.len(), 1);
        assert_eq!(semantics[0].label, "hello");
        tree.clear_dirty();
        assert_eq!(tree.dirty_nodes().count(), 0);

        let mut updated = node;
        updated.cache_key = Some(11);
        tree.upsert(updated);
        assert_eq!(tree.dirty_nodes().count(), 1);
    }

    #[test]
    fn display_list_keeps_renderer_independent_commands() {
        let mut list = DisplayList::default();
        list.push(DisplayCommand::RoundedRect {
            id: UiId(7),
            rect: UiRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            },
            radius: 8.0,
            color: DesktopTheme::light().panel,
        });
        list.push(DisplayCommand::Text {
            id: UiId(8),
            origin: (8.0, 16.0),
            runs: vec![DisplayTextRun {
                text: "hello".to_string(),
                font_stack: TextEngineConfig::desktop_default().font_stack,
                size_px: 15.0,
                color: DesktopTheme::light().text,
                attrs: TextAttributes::default(),
            }],
        });
        assert_eq!(list.commands.len(), 2);
    }

    #[test]
    fn glyph_atlas_lifecycle_records_font_changes_and_evictions() {
        let mut atlas = GlyphAtlasLifecycle::default();
        atlas.note_glyphs_uploaded(10, 4096);
        assert_eq!(atlas.glyph_count, 10);
        atlas.note_font_stack_changed();
        assert_eq!(atlas.font_epoch, 1);
        assert_eq!(atlas.glyph_count, 0);
        atlas.note_glyphs_uploaded(3, 1024);
        atlas.evict_all();
        assert_eq!(atlas.evictions, 1);
        assert_eq!(atlas.glyph_count, 0);
    }

    #[test]
    fn ime_state_tracks_preedit_and_commit() {
        let mut ime = ImeState::default();
        ime.apply_preedit("かな", Some((0, 6)));
        assert!(ime.active);
        assert_eq!(ime.commit(), "かな");
        assert!(!ime.active);
        assert!(ime.preedit.is_empty());
    }

    #[test]
    fn virtual_list_and_surface_cache_report_minimal_invalidation() {
        let range = VirtualListState {
            total_items: 100,
            first_visible: 10,
            visible_count: 5,
            overscan: 2,
        }
        .materialized_range();
        assert_eq!(range, 8..17);

        let mut cache = SurfaceRenderCache::new(UiId(42));
        let flags = cache.update_keys(1, 2, 3);
        assert!(flags.layout && flags.paint && flags.semantics);
        let flags = cache.update_keys(1, 9, 3);
        assert!(!flags.layout && flags.paint && !flags.semantics);
        assert_eq!(cache.invalidation_epoch, 2);
    }

    #[test]
    fn preferences_cover_font_scale_and_reduced_motion() {
        let prefs = UiPreferences {
            font_scale: 9.0,
            reduced_motion: true,
            ..UiPreferences::default()
        };
        assert_eq!(prefs.clamped_font_scale(), 1.60);
        assert_eq!(prefs.animation_duration_ms(180), 0);
    }
}
