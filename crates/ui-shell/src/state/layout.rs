#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Grid,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeTarget {
    Sidebar,
    RightPanel,
    PreviewPanel,
    PaneDivider,
    PropertiesDivider,
    ListColumnBoundary {
        pane: usize,
        left: ListColumnKind,
        right: ListColumnKind,
    },
}

#[derive(Clone, Debug)]
pub struct ResizeState {
    pub target: ResizeTarget,
    pub start_mouse_x: f32,
    pub start_mouse_y: f32,
    pub start_value: f32,
    pub start_value2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
pub enum PreviewPosition {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

/// Position relative to the preview panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
pub enum PropertiesPosition {
    #[default]
    BelowPreview,
    AbovePreview,
    LeftOfPreview,
    RightOfPreview,
}

impl PropertiesPosition {
    pub fn i18n_key(self) -> &'static str {
        match self {
            PropertiesPosition::BelowPreview => "properties-position-below-preview",
            PropertiesPosition::AbovePreview => "properties-position-above-preview",
            PropertiesPosition::LeftOfPreview => "properties-position-left-of-preview",
            PropertiesPosition::RightOfPreview => "properties-position-right-of-preview",
        }
    }

    pub fn is_horizontal(&self) -> bool {
        matches!(
            self,
            PropertiesPosition::LeftOfPreview | PropertiesPosition::RightOfPreview
        )
    }

    pub fn properties_first(&self) -> bool {
        matches!(
            self,
            PropertiesPosition::AbovePreview | PropertiesPosition::LeftOfPreview
        )
    }

    pub const ALL: [PropertiesPosition; 4] = [
        PropertiesPosition::BelowPreview,
        PropertiesPosition::AbovePreview,
        PropertiesPosition::LeftOfPreview,
        PropertiesPosition::RightOfPreview,
    ];
}

impl std::fmt::Display for PropertiesPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

impl PreviewPosition {
    pub fn i18n_key(self) -> &'static str {
        match self {
            PreviewPosition::Right => "preview-position-right",
            PreviewPosition::Left => "preview-position-left",
            PreviewPosition::Top => "preview-position-top",
            PreviewPosition::Bottom => "preview-position-bottom",
        }
    }

    pub const ALL: [PreviewPosition; 4] = [
        PreviewPosition::Right,
        PreviewPosition::Left,
        PreviewPosition::Top,
        PreviewPosition::Bottom,
    ];
}

impl std::fmt::Display for PreviewPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Date,
    ModifiedDate,
    CreatedDate,
    Size,
    FileType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListColumnKind {
    Name,
    Size,
    Date,
}

impl ListColumnKind {
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::Name => "pane-column-name",
            Self::Size => "pane-column-size",
            Self::Date => "pane-column-date",
        }
    }

    pub fn sort_key(self) -> SortKey {
        match self {
            Self::Name => SortKey::Name,
            Self::Size => SortKey::Size,
            Self::Date => SortKey::Date,
        }
    }

    pub fn is_flexible(self) -> bool {
        matches!(self, Self::Name)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListColumnSpec {
    pub kind: ListColumnKind,
    pub width: f32,
    pub visible: bool,
    pub sortable: bool,
}

impl ListColumnSpec {
    pub fn default_columns() -> Vec<Self> {
        vec![
            Self {
                kind: ListColumnKind::Name,
                width: 0.0,
                visible: true,
                sortable: true,
            },
            Self {
                kind: ListColumnKind::Size,
                width: 80.0,
                visible: true,
                sortable: true,
            },
            Self {
                kind: ListColumnKind::Date,
                width: 140.0,
                visible: true,
                sortable: true,
            },
        ]
    }
}

impl std::fmt::Display for SortKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

impl SortKey {
    pub fn i18n_key(self) -> &'static str {
        match self {
            SortKey::Name => "sort-key-name",
            SortKey::Date => "sort-key-date",
            SortKey::ModifiedDate => "sort-key-modified-date",
            SortKey::CreatedDate => "sort-key-created-date",
            SortKey::Size => "sort-key-size",
            SortKey::FileType => "sort-key-file-type",
        }
    }

    pub const ALL: [SortKey; 6] = [
        SortKey::Name,
        SortKey::Date,
        SortKey::ModifiedDate,
        SortKey::CreatedDate,
        SortKey::Size,
        SortKey::FileType,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}
