use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::frame::{CursorShape, Rgb};
use crate::protocol::{ALT, CTRL, Key, KeyCode, SHIFT};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    Normal,
    Leader,
    Vim,
    Tree,
    Theme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Action {
    EnterLeader,
    SessionTree,
    NewWindow,
    NewSession,
    SetSessionRoot,
    RenameSession,
    RenameWindow,
    SplitHorizontal,
    SplitVertical,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    ResizePaneLeft,
    ResizePaneDown,
    ResizePaneUp,
    ResizePaneRight,
    ZoomPane,
    BreakPane,
    SwapWindowLeft,
    SwapWindowRight,
    JumpToBell,
    KillPane,
    KillSession,
    LeaderCancel,
    SelectWindow(u8),
    EnterVim,
    EnterVimJump,
    Detach,
    RefreshClient,
    TreeDown,
    TreeUp,
    TreeChoose,
    TreeCancel,
    TreeSelect(u8),
    TreeExpand,
    TreeCollapse,
    TreeToggle,
    ThemePicker,
    ThemeNext,
    ThemePrevious,
    ThemeChoose,
    ThemeCancel,
    ThemeSelect(u8),
    CursorLeft,
    CursorDown,
    CursorUp,
    CursorRight,
    CursorDown3,
    CursorUp3,
    CursorDown10,
    CursorUp10,
    HalfPageDown,
    HalfPageUp,
    HalfPageDownCenter,
    HalfPageUpCenter,
    WordForward,
    BigWordForward,
    WordEnd,
    BigWordEnd,
    WordBackward,
    BigWordBackward,
    LineStart,
    FirstNonBlank,
    LineEnd,
    GoTop,
    GoBottom,
    FindForward,
    FindBackward,
    TillForward,
    TillBackward,
    RepeatFindForward,
    RepeatFindBackward,
    SearchForward,
    SearchBackward,
    RepeatSearch,
    RepeatSearchReverse,
    JumpCharacter,
    JumpOlder,
    JumpNewer,
    Visual,
    VisualLine,
    VisualBlock,
    Yank,
    YankToLineEnd,
    Escape,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bindings {
    by_mode: HashMap<Mode, HashMap<Key, Action>>,
}

/// How a pending bell shows itself on a window label.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BellStyle {
    /// A highlight sweeps the label until its window is selected.
    #[default]
    Shimmer,
    /// The label rests on the bell colour without moving. Nothing animates, so
    /// the daemon sends no frames while a bell waits.
    Steady,
    /// No visual bell at all. The bell is still recorded, so `jump-to-bell`
    /// finds the pane that rang it.
    None,
}

/// Which glyphs mux draws its sidebar with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Glyphs {
    /// The separator and program icons from the private-use code points of
    /// the font mux was designed with.
    #[default]
    Font,
    /// Box drawing and short text labels, which every terminal font has.
    Text,
}

/// Everything the configuration file decides, which a client hands to the
/// daemon when it attaches.
#[derive(Clone, Debug)]
pub struct Settings {
    pub bindings: Bindings,
    pub clipboard_command: Vec<String>,
    pub theme: Theme,
    /// The command the theme picker runs to switch theme, given the chosen name
    /// as its last argument. It owns the switch for every program on the
    /// machine; mux only asks for it and is themed back by the `set-theme` the
    /// command sends.
    pub theme_command: Vec<String>,
    /// Where the picker looks for themes: one directory per theme, each holding
    /// the `mux.toml` that describes its colours.
    pub theme_directory: Option<PathBuf>,
    pub mouse: bool,
    pub bell_style: BellStyle,
    pub default_cursor_shape: CursorShape,
    /// Overrides for what the client works out about its terminal from the
    /// environment; `None` leaves it to the detection.
    pub truecolor: Option<bool>,
    pub styled_underlines: Option<bool>,
    pub glyphs: Glyphs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bindings: Bindings::defaults(),
            clipboard_command: default_clipboard(),
            theme: Theme::default(),
            theme_command: default_theme_command(),
            theme_directory: default_theme_directory(),
            mouse: false,
            bell_style: BellStyle::default(),
            default_cursor_shape: CursorShape::default(),
            truecolor: None,
            styled_underlines: None,
            glyphs: Glyphs::default(),
        }
    }
}

impl Settings {
    /// Loads `path`, or the default configuration file when no path is given.
    ///
    /// An explicit path must exist; the default one is optional, so a fresh
    /// machine keeps the built-in bindings.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => match default_config_path().filter(|path| path.is_file()) {
                Some(path) => path,
                None => return Ok(Self::default()),
            },
        };
        let config: FileConfig = read_toml(&path, "config")?;
        for (name, command) in [
            ("clipboard_command", &config.clipboard_command),
            ("theme_command", &config.theme_command),
        ] {
            if command.first().is_none_or(String::is_empty) {
                bail!("{name} must contain a command name");
            }
        }
        let mut bindings = Bindings::defaults();
        for (mode, values) in [
            (Mode::Normal, config.normal),
            (Mode::Leader, config.leader),
            (Mode::Vim, config.vim),
            (Mode::Tree, config.tree),
            (Mode::Theme, config.themes),
        ] {
            bindings.apply(mode, values)?;
        }
        let base = match config.theme {
            Some(theme_path) => Theme::load(Path::new(&theme_path))?.palette,
            None => Palette::default(),
        };
        let palette = base.with_overrides(config.variant, config.palette)?;
        Ok(Self {
            bindings,
            clipboard_command: config.clipboard_command,
            theme: Theme::from_palette(palette),
            theme_command: config.theme_command,
            theme_directory: config
                .theme_directory
                .map(PathBuf::from)
                .or_else(default_theme_directory),
            mouse: config.mouse,
            bell_style: config.bell_style,
            default_cursor_shape: config.default_cursor_shape,
            truecolor: config.truecolor,
            styled_underlines: config.styled_underlines,
            glyphs: config.glyphs,
        })
    }
}

/// A theme, as the `theme` command writes it: the colours that define the
/// scheme, with no mention of what any of them is for.
///
/// Every program on the machine is themed from these same roles, so this is the
/// vocabulary a theme is written and read in. Which part of mux wears which is
/// [`Theme`]'s business, not the theme file's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    pub variant: Variant,
    pub background: Rgb,
    pub foreground: Rgb,
    pub surface: Rgb,
    pub surface_raised: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub secondary: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub selection: Rgb,
    pub diff_add: Rgb,
    pub diff_delete: Rgb,
    pub diff_change: Rgb,
}

/// Whether a theme is painted on a dark or a light ground.
///
/// It decides what colour text goes on a saturated fill: a dark theme writes in
/// its own background, and a light one in white, because that is the only way
/// either stays legible on `accent` or `secondary`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    #[default]
    Dark,
    Light,
}

/// The colours mux paints with, worked out from a [`Palette`].
///
/// This is the mapping from what a colour means to where it goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    /// What the theme file said, which is what the theme picker shows.
    pub palette: Palette,
    pub bar_inactive: Rgb,
    pub bar_active: Rgb,
    /// The tile behind the state dot at the top of the bar, one colour per mode.
    pub state_normal: Rgb,
    /// The dot itself while normal; every other state writes in `ink`.
    pub state_normal_dot: Rgb,
    pub state_leader: Rgb,
    pub state_passthrough: Rgb,
    pub state_vim: Rgb,
    pub bar_label_foreground: Rgb,
    pub cursor: Rgb,
    pub divider: Rgb,
    pub panel_background: Rgb,
    pub panel_selected: Rgb,
    pub panel_foreground: Rgb,
    pub panel_row_foreground: Rgb,
    pub panel_heading: Rgb,
    pub popup_background: Rgb,
    pub popup_text: Rgb,
    pub popup_accent: Rgb,
    pub popup_warning: Rgb,
    pub vim_selection: Rgb,
    pub vim_jump: Rgb,
    pub vim_search: Rgb,
    pub vim_search_current: Rgb,
    pub bell_base: Rgb,
    pub bell_highlight: Rgb,
    pub bell_text: Rgb,
}

const WHITE: Rgb = (0xff, 0xff, 0xff);

/// Mixes `amount` percent of `to` into `from`.
fn blend(from: Rgb, to: Rgb, amount: u16) -> Rgb {
    let mix = |from: u8, to: u8| {
        ((from as u16 * (100 - amount) + to as u16 * amount) / 100).min(255) as u8
    };
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

impl Palette {
    /// What to write on a fill of one of this palette's saturated colours.
    fn ink(&self) -> Rgb {
        match self.variant {
            Variant::Dark => self.background,
            Variant::Light => WHITE,
        }
    }
}

impl Theme {
    pub fn from_palette(palette: Palette) -> Self {
        Self {
            palette,
            bar_inactive: palette.surface_raised,
            bar_active: palette.secondary,
            // Normal sits back into the bar, so the dot only speaks up once a
            // mode takes the keys: the bell accent once a second leader has
            // handed the next one over wholesale, and a colour per mode that
            // intercepts.
            state_normal: palette.surface,
            state_normal_dot: palette.muted,
            state_leader: palette.secondary,
            state_passthrough: palette.accent,
            state_vim: palette.accent,
            bar_label_foreground: palette.ink(),
            cursor: palette.accent,
            divider: palette.surface_raised,
            panel_background: palette.surface,
            panel_selected: palette.selection,
            panel_foreground: palette.foreground,
            panel_row_foreground: palette.foreground,
            panel_heading: palette.muted,
            popup_background: palette.surface,
            popup_text: palette.foreground,
            popup_accent: palette.success,
            popup_warning: palette.warning,
            vim_selection: palette.selection,
            vim_jump: palette.secondary,
            vim_search: palette.surface_raised,
            vim_search_current: palette.warning,
            // A bell uses the palette's accent colour, and the shimmer sweeps
            // it towards the text colour.
            bell_base: palette.accent,
            bell_highlight: blend(palette.accent, palette.foreground, 45),
            bell_text: palette.ink(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePalette {
    background: Option<String>,
    foreground: Option<String>,
    surface: Option<String>,
    surface_raised: Option<String>,
    muted: Option<String>,
    accent: Option<String>,
    secondary: Option<String>,
    success: Option<String>,
    warning: Option<String>,
    danger: Option<String>,
    selection: Option<String>,
    diff_add: Option<String>,
    diff_delete: Option<String>,
    diff_change: Option<String>,
}

/// A theme file: the variant, and the palette under it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileThemeConfig {
    variant: Option<Variant>,
    #[serde(default)]
    palette: FilePalette,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    theme: Option<String>,
    #[serde(default)]
    normal: HashMap<String, String>,
    #[serde(default)]
    leader: HashMap<String, String>,
    #[serde(default)]
    vim: HashMap<String, String>,
    #[serde(default)]
    tree: HashMap<String, String>,
    #[serde(default)]
    themes: HashMap<String, String>,
    #[serde(default = "default_clipboard")]
    clipboard_command: Vec<String>,
    #[serde(default = "default_theme_command")]
    theme_command: Vec<String>,
    theme_directory: Option<String>,
    /// Off by default: capturing the mouse takes click-to-select away from the
    /// terminal, which is a trade only the user can decide to make.
    #[serde(default)]
    mouse: bool,
    #[serde(default)]
    bell_style: BellStyle,
    #[serde(default)]
    default_cursor_shape: CursorShape,
    /// Whether the terminal renders 24-bit colour, when the environment
    /// does not say so correctly.
    truecolor: Option<bool>,
    /// Whether the terminal renders curly, dotted and dashed underlines and
    /// underline colours, when the environment does not say so correctly.
    styled_underlines: Option<bool>,
    /// `text` for terminals whose font lacks mux's private-use glyphs.
    #[serde(default)]
    glyphs: Glyphs,
    variant: Option<Variant>,
    #[serde(default)]
    palette: FilePalette,
}

fn default_clipboard() -> Vec<String> {
    vec!["yank".into()]
}

fn default_theme_command() -> Vec<String> {
    vec!["theme".into()]
}

/// `$XDG_CONFIG_HOME/mux/config.toml`, falling back to `~/.config`.
pub fn default_config_path() -> Option<PathBuf> {
    default_config_home().map(|home| home.join("mux/config.toml"))
}

/// `$XDG_CONFIG_HOME/theme/themes`, which is the layout the `theme` command
/// keeps: one directory per theme beside the `current` link that names the one
/// in use.
fn default_theme_directory() -> Option<PathBuf> {
    default_config_home().map(|home| home.join("theme/themes"))
}

fn default_config_home() -> Option<PathBuf> {
    config_home_in(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn config_home_in(config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    config_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".config")))
}

impl Bindings {
    pub fn defaults() -> Self {
        let mut bindings = Self {
            by_mode: HashMap::new(),
        };
        let mut bind = |mode, key: &str, action| {
            bindings
                .by_mode
                .entry(mode)
                .or_default()
                .insert(parse_key(key).unwrap(), action);
        };

        bind(Mode::Normal, "Alt-a", Action::EnterLeader);
        bind(Mode::Normal, "Alt-s", Action::SessionTree);
        bind(Mode::Normal, "Alt-c", Action::ThemePicker);
        bind(Mode::Normal, "Alt-t", Action::NewWindow);
        bind(Mode::Normal, "Alt-Shift-t", Action::NewSession);
        bind(Mode::Normal, "Alt-Shift-r", Action::SetSessionRoot);
        bind(Mode::Normal, "Alt-w", Action::EnterVim);
        bind(Mode::Normal, "Alt-d", Action::EnterVimJump);
        bind(Mode::Normal, "Alt-f", Action::ZoomPane);
        bind(Mode::Vim, "Alt-a", Action::EnterLeader);
        bind(Mode::Vim, "Alt-d", Action::JumpCharacter);
        for number in 1..=9 {
            let key = format!("Alt-{number}");
            bind(Mode::Normal, &key, Action::SelectWindow(number));
            bind(Mode::Vim, &key, Action::SelectWindow(number));
        }

        bind(Mode::Leader, "$", Action::RenameSession);
        bind(Mode::Leader, ",", Action::RenameWindow);
        bind(Mode::Leader, "-", Action::SplitHorizontal);
        bind(Mode::Leader, "|", Action::SplitVertical);
        bind(Mode::Leader, "d", Action::Detach);
        bind(Mode::Leader, "r", Action::RefreshClient);
        bind(Mode::Leader, "b", Action::JumpToBell);
        bind(Mode::Leader, "x", Action::KillPane);
        bind(Mode::Leader, "Left", Action::FocusPaneLeft);
        bind(Mode::Leader, "Down", Action::FocusPaneDown);
        bind(Mode::Leader, "Up", Action::FocusPaneUp);
        bind(Mode::Leader, "Right", Action::FocusPaneRight);
        bind(Mode::Leader, "Ctrl-Left", Action::ResizePaneLeft);
        bind(Mode::Leader, "Ctrl-Down", Action::ResizePaneDown);
        bind(Mode::Leader, "Ctrl-Up", Action::ResizePaneUp);
        bind(Mode::Leader, "Ctrl-Right", Action::ResizePaneRight);
        bind(Mode::Leader, "!", Action::BreakPane);
        bind(Mode::Leader, "<", Action::SwapWindowLeft);
        bind(Mode::Leader, ">", Action::SwapWindowRight);
        bind(Mode::Leader, "Escape", Action::LeaderCancel);

        bind(Mode::Tree, "j", Action::TreeDown);
        bind(Mode::Tree, "Down", Action::TreeDown);
        bind(Mode::Tree, "k", Action::TreeUp);
        bind(Mode::Tree, "Up", Action::TreeUp);
        bind(Mode::Tree, "Enter", Action::TreeChoose);
        bind(Mode::Tree, "Escape", Action::TreeCancel);
        bind(Mode::Tree, "l", Action::TreeExpand);
        bind(Mode::Tree, "Right", Action::TreeExpand);
        bind(Mode::Tree, "h", Action::TreeCollapse);
        bind(Mode::Tree, "Left", Action::TreeCollapse);
        bind(Mode::Tree, " ", Action::TreeToggle);
        bind(Mode::Tree, "x", Action::KillSession);
        bind(Mode::Tree, "Alt-a", Action::EnterLeader);
        bind(Mode::Tree, "Alt-s", Action::SessionTree);
        for number in 1..=9 {
            bind(Mode::Tree, &number.to_string(), Action::TreeSelect(number));
        }
        bind(Mode::Tree, "0", Action::TreeSelect(10));
        for offset in 1..26 {
            if offset == b's' - b'a' {
                continue;
            }
            bind(
                Mode::Tree,
                &format!("Alt-{}", char::from(b'a' + offset)),
                Action::TreeSelect(10 + offset),
            );
        }

        // The themes sit in a strip, so both axes walk along it.
        for key in ["l", "Right", "j", "Down", "Tab"] {
            bind(Mode::Theme, key, Action::ThemeNext);
        }
        for key in ["h", "Left", "k", "Up", "Shift-Tab"] {
            bind(Mode::Theme, key, Action::ThemePrevious);
        }
        bind(Mode::Theme, "Enter", Action::ThemeChoose);
        bind(Mode::Theme, "Escape", Action::ThemeCancel);
        bind(Mode::Theme, "q", Action::ThemeCancel);
        for number in 1..=9 {
            bind(
                Mode::Theme,
                &number.to_string(),
                Action::ThemeSelect(number),
            );
        }

        bind(Mode::Vim, "h", Action::CursorLeft);
        bind(Mode::Vim, "Left", Action::CursorLeft);
        bind(Mode::Vim, "j", Action::CursorDown);
        bind(Mode::Vim, "Down", Action::CursorDown);
        bind(Mode::Vim, "k", Action::CursorUp);
        bind(Mode::Vim, "Up", Action::CursorUp);
        bind(Mode::Vim, "l", Action::CursorRight);
        bind(Mode::Vim, "Right", Action::CursorRight);
        bind(Mode::Vim, "Ctrl-d", Action::HalfPageDown);
        bind(Mode::Vim, "Ctrl-u", Action::HalfPageUp);
        bind(Mode::Vim, "w", Action::WordForward);
        bind(Mode::Vim, "W", Action::BigWordForward);
        bind(Mode::Vim, "e", Action::WordEnd);
        bind(Mode::Vim, "E", Action::BigWordEnd);
        bind(Mode::Vim, "b", Action::WordBackward);
        bind(Mode::Vim, "B", Action::BigWordBackward);
        bind(Mode::Vim, "0", Action::LineStart);
        bind(Mode::Vim, "^", Action::FirstNonBlank);
        bind(Mode::Vim, "$", Action::LineEnd);
        bind(Mode::Vim, "g", Action::GoTop);
        bind(Mode::Vim, "G", Action::GoBottom);
        bind(Mode::Vim, "f", Action::FindForward);
        bind(Mode::Vim, "F", Action::FindBackward);
        bind(Mode::Vim, "t", Action::TillForward);
        bind(Mode::Vim, "T", Action::TillBackward);
        bind(Mode::Vim, ";", Action::RepeatFindForward);
        bind(Mode::Vim, ",", Action::RepeatFindBackward);
        bind(Mode::Vim, "/", Action::SearchForward);
        bind(Mode::Vim, "?", Action::SearchBackward);
        bind(Mode::Vim, "n", Action::RepeatSearch);
        bind(Mode::Vim, "N", Action::RepeatSearchReverse);
        bind(Mode::Vim, " ", Action::JumpCharacter);
        bind(Mode::Vim, "Ctrl-o", Action::JumpOlder);
        bind(Mode::Vim, "Ctrl-l", Action::JumpNewer);
        bind(Mode::Vim, "Ctrl-i", Action::JumpNewer);
        bind(Mode::Vim, "Tab", Action::JumpNewer);
        bind(Mode::Vim, "v", Action::Visual);
        bind(Mode::Vim, "V", Action::VisualLine);
        bind(Mode::Vim, "Ctrl-v", Action::VisualBlock);
        bind(Mode::Vim, "y", Action::Yank);
        bind(Mode::Vim, "Y", Action::YankToLineEnd);
        bind(Mode::Vim, "Escape", Action::Escape);
        bindings
    }

    pub fn get(&self, mode: Mode, key: &Key) -> Option<Action> {
        self.by_mode.get(&mode)?.get(key).copied()
    }

    fn apply(&mut self, mode: Mode, values: HashMap<String, String>) -> Result<()> {
        for (key_name, action_name) in values {
            let key = parse_key(&key_name)
                .with_context(|| format!("invalid {mode:?} binding key {key_name:?}"))?;
            if action_name == "unbind" {
                self.by_mode.entry(mode).or_default().remove(&key);
                continue;
            }
            let action = parse_action(&action_name)
                .with_context(|| format!("invalid {mode:?} action for key {key_name:?}"))?;
            if !action.valid_in(mode) {
                bail!(
                    "action {action_name:?} cannot be used in {} mode",
                    mode.name()
                );
            }
            self.by_mode.entry(mode).or_default().insert(key, action);
        }
        Ok(())
    }
}

impl Default for Palette {
    /// The dusk palette, which is the one mux was drawn against.
    fn default() -> Self {
        Self {
            variant: Variant::Dark,
            background: (0x24, 0x1e, 0x2d),
            foreground: (0xec, 0xe7, 0xf2),
            surface: (0x2e, 0x27, 0x39),
            surface_raised: (0x4a, 0x41, 0x58),
            muted: (0x96, 0x8a, 0xa6),
            accent: (0x9f, 0xa8, 0xf2),
            secondary: (0xcb, 0xa3, 0xd2),
            success: (0x8f, 0xd0, 0xa0),
            warning: (0xe3, 0xb4, 0x6b),
            danger: (0xf2, 0x8c, 0xa0),
            selection: (0x3f, 0x35, 0x52),
            diff_add: (0x2c, 0x44, 0x34),
            diff_delete: (0x4d, 0x2b, 0x38),
            diff_change: (0x4a, 0x40, 0x30),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_palette(Palette::default())
    }
}

impl Theme {
    pub fn load(path: &Path) -> Result<Self> {
        let config: FileThemeConfig = read_toml(path, "theme config")?;
        let palette = Palette::default().with_overrides(config.variant, config.palette)?;
        Ok(Self::from_palette(palette))
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path, what: &str) -> Result<T> {
    let source =
        fs::read_to_string(path).with_context(|| format!("read {what} {}", path.display()))?;
    toml::from_str(&source).with_context(|| format!("parse {what} {}", path.display()))
}

impl Palette {
    fn with_overrides(mut self, variant: Option<Variant>, values: FilePalette) -> Result<Self> {
        if let Some(variant) = variant {
            self.variant = variant;
        }
        macro_rules! apply_colors {
            ($($field:ident),*) => {$(
                if let Some(value) = values.$field {
                    self.$field = parse_color(&value)
                        .with_context(|| format!("invalid color {}", stringify!($field)))?;
                }
            )*};
        }
        apply_colors!(
            background,
            foreground,
            surface,
            surface_raised,
            muted,
            accent,
            secondary,
            success,
            warning,
            danger,
            selection,
            diff_add,
            diff_delete,
            diff_change
        );
        Ok(self)
    }
}

fn parse_color(value: &str) -> Result<Rgb> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 {
        bail!("expected #rrggbb or rrggbb")
    }
    // Byte slicing below would panic inside a multi-byte character.
    if !hex.is_ascii() {
        bail!("expected hexadecimal color")
    }
    let channel = |start: usize| {
        u8::from_str_radix(&hex[start..start + 2], 16).context("expected hexadecimal color")
    };
    Ok((channel(0)?, channel(2)?, channel(4)?))
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Leader => "leader",
            Self::Vim => "vim",
            Self::Tree => "tree",
            Self::Theme => "theme",
        }
    }
}

impl Action {
    fn valid_in(self, mode: Mode) -> bool {
        match mode {
            Mode::Normal => matches!(
                self,
                Self::EnterLeader
                    | Self::SessionTree
                    | Self::NewWindow
                    | Self::NewSession
                    | Self::SetSessionRoot
                    | Self::SelectWindow(_)
                    | Self::EnterVim
                    | Self::EnterVimJump
                    | Self::Detach
                    | Self::RefreshClient
                    | Self::ThemePicker
                    | Self::ZoomPane
            ),
            Mode::Leader => matches!(
                self,
                Self::RenameSession
                    | Self::RenameWindow
                    | Self::SplitHorizontal
                    | Self::SplitVertical
                    | Self::FocusPaneLeft
                    | Self::FocusPaneDown
                    | Self::FocusPaneUp
                    | Self::FocusPaneRight
                    | Self::ResizePaneLeft
                    | Self::ResizePaneDown
                    | Self::ResizePaneUp
                    | Self::ResizePaneRight
                    | Self::BreakPane
                    | Self::SwapWindowLeft
                    | Self::SwapWindowRight
                    | Self::JumpToBell
                    | Self::KillPane
                    | Self::Detach
                    | Self::RefreshClient
                    | Self::LeaderCancel
                    | Self::ThemePicker
            ),
            Mode::Tree => matches!(
                self,
                Self::EnterLeader
                    | Self::SessionTree
                    | Self::TreeDown
                    | Self::TreeUp
                    | Self::TreeChoose
                    | Self::TreeCancel
                    | Self::TreeSelect(_)
                    | Self::TreeExpand
                    | Self::TreeCollapse
                    | Self::TreeToggle
                    | Self::KillSession
                    | Self::ThemePicker
            ),
            Mode::Theme => matches!(
                self,
                Self::ThemePicker
                    | Self::ThemeNext
                    | Self::ThemePrevious
                    | Self::ThemeChoose
                    | Self::ThemeCancel
                    | Self::ThemeSelect(_)
            ),
            // Vim mode takes its own motions, plus the few actions that reach
            // past the pane it is reading.
            Mode::Vim => {
                matches!(
                    self,
                    Self::EnterLeader | Self::SelectWindow(_) | Self::RefreshClient
                ) || ![Mode::Normal, Mode::Leader, Mode::Tree, Mode::Theme]
                    .into_iter()
                    .any(|mode| self.valid_in(mode))
            }
        }
    }
}

/// Every action a binding can name, besides the numbered ones.
const ACTION_NAMES: &[(&str, Action)] = &[
    ("leader", Action::EnterLeader),
    ("session-tree", Action::SessionTree),
    ("new-window", Action::NewWindow),
    ("new-session", Action::NewSession),
    ("set-session-root", Action::SetSessionRoot),
    ("rename-session", Action::RenameSession),
    ("rename-window", Action::RenameWindow),
    ("split-horizontal", Action::SplitHorizontal),
    ("split-vertical", Action::SplitVertical),
    ("focus-pane-left", Action::FocusPaneLeft),
    ("focus-pane-down", Action::FocusPaneDown),
    ("focus-pane-up", Action::FocusPaneUp),
    ("focus-pane-right", Action::FocusPaneRight),
    ("resize-pane-left", Action::ResizePaneLeft),
    ("resize-pane-down", Action::ResizePaneDown),
    ("resize-pane-up", Action::ResizePaneUp),
    ("resize-pane-right", Action::ResizePaneRight),
    ("focus-mode", Action::ZoomPane),
    ("break-pane", Action::BreakPane),
    ("swap-window-left", Action::SwapWindowLeft),
    ("swap-window-right", Action::SwapWindowRight),
    ("jump-to-bell", Action::JumpToBell),
    ("kill-pane", Action::KillPane),
    ("kill-session", Action::KillSession),
    ("leader-cancel", Action::LeaderCancel),
    ("enter-vim", Action::EnterVim),
    ("enter-vim-jump", Action::EnterVimJump),
    ("detach", Action::Detach),
    ("refresh-client", Action::RefreshClient),
    ("tree-down", Action::TreeDown),
    ("tree-up", Action::TreeUp),
    ("tree-choose", Action::TreeChoose),
    ("tree-cancel", Action::TreeCancel),
    ("tree-expand", Action::TreeExpand),
    ("tree-collapse", Action::TreeCollapse),
    ("tree-toggle", Action::TreeToggle),
    ("theme-picker", Action::ThemePicker),
    ("theme-next", Action::ThemeNext),
    ("theme-previous", Action::ThemePrevious),
    ("theme-choose", Action::ThemeChoose),
    ("theme-cancel", Action::ThemeCancel),
    ("left", Action::CursorLeft),
    ("down", Action::CursorDown),
    ("up", Action::CursorUp),
    ("right", Action::CursorRight),
    ("down-3", Action::CursorDown3),
    ("up-3", Action::CursorUp3),
    ("down-10", Action::CursorDown10),
    ("up-10", Action::CursorUp10),
    ("half-page-down", Action::HalfPageDown),
    ("half-page-up", Action::HalfPageUp),
    ("half-page-down-center", Action::HalfPageDownCenter),
    ("half-page-up-center", Action::HalfPageUpCenter),
    ("word-forward", Action::WordForward),
    ("big-word-forward", Action::BigWordForward),
    ("word-end", Action::WordEnd),
    ("big-word-end", Action::BigWordEnd),
    ("word-backward", Action::WordBackward),
    ("big-word-backward", Action::BigWordBackward),
    ("line-start", Action::LineStart),
    ("first-nonblank", Action::FirstNonBlank),
    ("line-end", Action::LineEnd),
    ("go-top", Action::GoTop),
    ("go-bottom", Action::GoBottom),
    ("find-forward", Action::FindForward),
    ("find-backward", Action::FindBackward),
    ("till-forward", Action::TillForward),
    ("till-backward", Action::TillBackward),
    ("repeat-find-forward", Action::RepeatFindForward),
    ("repeat-find-backward", Action::RepeatFindBackward),
    ("search-forward", Action::SearchForward),
    ("search-backward", Action::SearchBackward),
    ("repeat-search", Action::RepeatSearch),
    ("repeat-search-reverse", Action::RepeatSearchReverse),
    ("jump-character", Action::JumpCharacter),
    ("jump-older", Action::JumpOlder),
    ("jump-newer", Action::JumpNewer),
    ("visual", Action::Visual),
    ("visual-line", Action::VisualLine),
    ("visual-block", Action::VisualBlock),
    ("yank", Action::Yank),
    ("yank-to-line-end", Action::YankToLineEnd),
    ("escape", Action::Escape),
];

/// The actions that end in a number: their prefix, what the number counts,
/// its largest value, and the action it makes.
type NumberedAction = (&'static str, &'static str, u8, fn(u8) -> Action);
const NUMBERED_ACTIONS: [NumberedAction; 3] = [
    ("select-window-", "window number", 9, Action::SelectWindow),
    ("tree-select-", "tree line number", 35, Action::TreeSelect),
    ("theme-select-", "theme line number", 9, Action::ThemeSelect),
];

fn parse_action(value: &str) -> Result<Action> {
    if let Some((_, action)) = ACTION_NAMES.iter().find(|(name, _)| *name == value) {
        return Ok(*action);
    }
    for (prefix, what, last, action) in NUMBERED_ACTIONS {
        if let Some(number) = value.strip_prefix(prefix) {
            let number: u8 = number
                .parse()
                .with_context(|| format!("invalid {what} in action {value:?}"))?;
            if !(1..=last).contains(&number) {
                bail!("{what} must be from 1 through {last}")
            }
            return Ok(action(number));
        }
    }
    bail!("unknown action {value:?}")
}

pub fn parse_key(value: &str) -> Result<Key> {
    const MODIFIERS: [(&str, u8); 4] = [
        ("shift-", SHIFT),
        ("alt-", ALT),
        ("ctrl-", CTRL),
        ("control-", CTRL),
    ];
    let mut modifiers = 0;
    let mut name = value;
    while let Some((prefix, bit)) = MODIFIERS.iter().find(|(prefix, _)| {
        name.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    }) {
        modifiers |= bit;
        name = &name[prefix.len()..];
    }
    if name.is_empty() {
        bail!("missing key name")
    }
    let code = match name.to_ascii_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "escape" | "esc" => KeyCode::Escape,
        "backspace" => KeyCode::Backspace,
        "tab" if modifiers & SHIFT != 0 => {
            modifiers &= !SHIFT;
            KeyCode::BackTab
        }
        "tab" => KeyCode::Tab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        function if function.starts_with('f') && function.len() > 1 => {
            let number: u8 = function[1..]
                .parse()
                .context("function keys are F1 through F12")?;
            if !(1..=12).contains(&number) {
                bail!("function keys are F1 through F12");
            }
            KeyCode::F(number)
        }
        _ => {
            let mut chars = name.chars();
            let character = chars.next().context("missing key name")?;
            if chars.next().is_some() {
                bail!("unknown key name {name:?}")
            }
            KeyCode::Char(character)
        }
    };
    Ok(Key { code, modifiers })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads `source` as a config file, through a temporary file named `name`.
    fn load(name: &str, source: &str) -> Result<Settings> {
        let path = env::temp_dir().join(name);
        fs::write(&path, source).unwrap();
        let settings = Settings::load(Some(&path));
        fs::remove_file(&path).unwrap();
        settings
    }

    fn key(name: &str) -> Key {
        parse_key(name).unwrap()
    }

    #[test]
    fn function_keys_can_be_bound_with_modifiers() {
        assert_eq!(
            key("Ctrl-F12"),
            Key {
                code: KeyCode::F(12),
                modifiers: CTRL
            }
        );
        assert_eq!(key("F1").code, KeyCode::F(1));
        assert_eq!(key("f").code, KeyCode::Char('f'));
        assert!(parse_key("F0").is_err());
        assert!(parse_key("F13").is_err());
    }

    #[test]
    fn the_default_config_lives_under_the_config_home() {
        assert_eq!(
            config_home_in(Some(OsStr::new("/x/config")), Some(OsStr::new("/home/j"))),
            Some(PathBuf::from("/x/config"))
        );
        assert_eq!(
            config_home_in(None, Some(OsStr::new("/home/j"))),
            Some(PathBuf::from("/home/j/.config"))
        );
        assert_eq!(config_home_in(None, None), None);
    }

    #[test]
    fn an_empty_config_keeps_the_built_in_bindings() {
        let settings = load("mux-empty-config.toml", "").unwrap();
        assert_eq!(settings.clipboard_command, default_clipboard());
        assert_eq!(settings.theme, Theme::default());
        assert!(
            !settings.mouse,
            "the mouse stays with the terminal by default"
        );
        let bindings = settings.bindings;
        assert_eq!(
            bindings.get(Mode::Normal, &key("Alt-a")),
            Some(Action::EnterLeader)
        );
        assert_eq!(
            bindings.get(Mode::Normal, &key("Alt-f")),
            Some(Action::ZoomPane)
        );
    }

    #[test]
    fn the_theme_command_and_directory_can_be_configured() {
        let settings = load(
            "mux-theme-config.toml",
            r#"
                theme_command = ["theme", "--quiet"]
                theme_directory = "/opt/themes"
                [themes]
                g = "theme-select-1"
            "#,
        )
        .unwrap();
        assert_eq!(settings.theme_command, ["theme", "--quiet"]);
        assert_eq!(settings.theme_directory, Some(PathBuf::from("/opt/themes")));
        assert_eq!(
            settings.bindings.get(Mode::Theme, &key("g")),
            Some(Action::ThemeSelect(1))
        );
    }

    #[test]
    fn a_binding_only_takes_actions_its_mode_has() {
        for (mode, expected) in [
            (Mode::Theme, "cannot be used in theme mode"),
            (Mode::Vim, "cannot be used in vim mode"),
        ] {
            let error = Bindings::defaults()
                .apply(mode, HashMap::from([("n".into(), "new-window".into())]))
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn an_explicit_config_path_must_exist() {
        assert!(Settings::load(Some(Path::new("/nonexistent/mux.toml"))).is_err());
    }

    #[test]
    fn user_binding_replaces_and_unbinds_defaults() {
        let source = r#"
            [normal]
            Alt-s = "unbind"
            Alt-x = "session-tree"
            [leader]
            v = "split-vertical"
            [vim]
            "§" = "first-nonblank"
            "Ctrl-d" = "half-page-down-center"
            "Shift-Down" = "down-10"
        "#;
        let bindings = load("mux-binding-config.toml", source).unwrap().bindings;
        for (mode, name, expected) in [
            (Mode::Normal, "Alt-s", None),
            (Mode::Normal, "Alt-x", Some(Action::SessionTree)),
            (Mode::Leader, "v", Some(Action::SplitVertical)),
            (Mode::Vim, "§", Some(Action::FirstNonBlank)),
            (Mode::Vim, "Ctrl-d", Some(Action::HalfPageDownCenter)),
            (Mode::Vim, "Shift-Down", Some(Action::CursorDown10)),
            (Mode::Vim, "Alt-a", Some(Action::EnterLeader)),
            (Mode::Vim, "Alt-d", Some(Action::JumpCharacter)),
        ] {
            assert_eq!(bindings.get(mode, &key(name)), expected, "{name}");
        }
    }

    #[test]
    fn non_ascii_colors_are_an_error_not_a_panic() {
        assert!(parse_color("a€bc").is_err());
        assert!(parse_color("#a€bc").is_err());
    }

    #[test]
    fn color_overrides_accept_hash_or_plain_hex_and_name_a_bad_role() {
        let overrides = |source: &str| {
            let parsed: FileConfig = toml::from_str(source).unwrap();
            Palette::default().with_overrides(None, parsed.palette)
        };
        let palette = overrides(
            r##"
                [palette]
                secondary = "#112233"
                warning = "aabbcc"
            "##,
        )
        .unwrap();
        assert_eq!(palette.secondary, (0x11, 0x22, 0x33));
        assert_eq!(palette.warning, (0xaa, 0xbb, 0xcc));
        assert_eq!(palette.surface_raised, Palette::default().surface_raised);

        let error = overrides("[palette]\nsecondary = \"11223\"").unwrap_err();
        assert!(error.to_string().contains("secondary"));
    }

    #[test]
    fn the_theme_file_says_what_a_colour_is_and_mux_says_where_it_goes() {
        let path = env::temp_dir().join("mux-palette-theme.toml");
        fs::write(
            &path,
            r#"
                variant = "light"
                [palette]
                background = "fcf9f0"
                foreground = "000000"
                surface = "e8e8e8"
                surface_raised = "cccccc"
                secondary = "009393"
                selection = "c4ffff"
                warning = "664400"
            "#,
        )
        .unwrap();
        let theme = Theme::load(&path).unwrap();
        fs::remove_file(&path).unwrap();
        // One colour reaches every part of mux that wears it, without the file
        // having to name any of them.
        assert_eq!(theme.bar_active, (0x00, 0x93, 0x93));
        assert_eq!(theme.state_vim, Palette::default().accent);
        assert_eq!(theme.vim_jump, (0x00, 0x93, 0x93));
        assert_eq!(theme.divider, (0xcc, 0xcc, 0xcc));
        assert_eq!(theme.vim_search, (0xcc, 0xcc, 0xcc));
        assert_eq!(theme.panel_selected, (0xc4, 0xff, 0xff));
        assert_eq!(theme.vim_selection, (0xc4, 0xff, 0xff));
        assert_eq!(theme.popup_warning, (0x66, 0x44, 0x00));
        assert_eq!(theme.vim_search_current, (0x66, 0x44, 0x00));
        assert_eq!(theme.bell_base, Palette::default().accent);
        // A light theme writes on its saturated fills in white; a dark one uses
        // its own background.
        assert_eq!(theme.bar_label_foreground, WHITE);
        assert_eq!(
            Theme::default().bar_label_foreground,
            Palette::default().background
        );
    }

    #[test]
    fn default_cursor_shape_loads_and_rejects_unknown_shapes() {
        for (source, expected) in [
            ("", CursorShape::Bar),
            ("default_cursor_shape = \"bar\"", CursorShape::Bar),
            ("default_cursor_shape = \"block\"", CursorShape::Block),
            (
                "default_cursor_shape = \"underline\"",
                CursorShape::Underline,
            ),
        ] {
            let settings = load("mux-cursor-config.toml", source).unwrap();
            assert_eq!(settings.default_cursor_shape, expected);
        }
        assert!(
            load(
                "mux-cursor-config.toml",
                "default_cursor_shape = \"triangle\""
            )
            .is_err()
        );
    }

    #[test]
    fn bell_style_defaults_to_the_shimmer_and_names_a_bad_value() {
        for (source, expected) in [
            ("mouse = true", BellStyle::Shimmer),
            (r#"bell_style = "steady""#, BellStyle::Steady),
            (r#"bell_style = "none""#, BellStyle::None),
        ] {
            let parsed: FileConfig = toml::from_str(source).unwrap();
            assert_eq!(parsed.bell_style, expected);
        }
        let error = toml::from_str::<FileConfig>(r#"bell_style = "flash""#).unwrap_err();
        assert!(error.to_string().contains("bell_style"), "{error}");
    }
}
