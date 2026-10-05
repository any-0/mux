const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";
const CLIPBOARD_SELECTOR: &[u8] = b"cpqs01234567";

/// Longest DCS payload kept; anything beyond it is dropped.
const MAX_DCS_LEN: usize = 1024;

pub struct WrappedScreen<CB: crate::callbacks::Callbacks = ()> {
    pub screen: crate::screen::Screen,
    pub callbacks: CB,
    /// The intermediates, final character and payload of the DCS sequence
    /// being received.
    dcs: Option<(Vec<u8>, char, Vec<u8>)>,
}

impl<CB: crate::callbacks::Callbacks> WrappedScreen<CB> {
    pub fn new(rows: u16, cols: u16, scrollback_len: usize, callbacks: CB) -> Self {
        Self {
            screen: crate::screen::Screen::new(crate::grid::Size { rows, cols }, scrollback_len),
            callbacks,
            dcs: None,
        }
    }
}

impl<CB: crate::callbacks::Callbacks> vte::Perform for WrappedScreen<CB> {
    fn print(&mut self, c: char) {
        if c == '\u{fffd}' || ('\u{80}'..'\u{a0}').contains(&c) {
            self.callbacks.unhandled_char(&mut self.screen, c);
        } else {
            self.screen.text(c);
        }
    }

    fn execute(&mut self, b: u8) {
        match b {
            7 => self.callbacks.audible_bell(&mut self.screen),
            8 => self.screen.bs(),
            9 => self.screen.cht(1),
            10..=12 => self.screen.lf(),
            13 => self.screen.cr(),
            14 => self.screen.shift_out(true),
            15 => self.screen.shift_out(false),
            _ => self.callbacks.unhandled_control(&mut self.screen, b),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, b: u8) {
        if let [slot @ (b'(' | b')')] = intermediates {
            if self.screen.designate_charset(usize::from(*slot == b')'), b) {
                return;
            }
        }
        if !intermediates.is_empty() {
            self.callbacks.unhandled_escape(
                &mut self.screen,
                intermediates.first().copied(),
                intermediates.get(1).copied(),
                b,
            );
            return;
        }
        match b {
            b'7' => self.screen.decsc(),
            b'8' => self.screen.decrc(),
            b'=' => self.screen.set_application_keypad(true),
            b'>' => self.screen.set_application_keypad(false),
            b'D' => self.screen.lf(),
            b'E' => self.screen.nel(),
            b'H' => self.screen.hts(),
            b'M' => self.screen.ri(),
            b'c' => self.screen.ris(),
            b'g' => self.callbacks.visual_bell(&mut self.screen),
            _ => {
                self.callbacks
                    .unhandled_escape(&mut self.screen, None, None, b);
            }
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        let mut unhandled = |screen: &mut crate::screen::Screen| {
            self.callbacks.unhandled_csi(
                screen,
                intermediates.first().copied(),
                intermediates.get(1).copied(),
                &params.iter().collect::<Vec<_>>(),
                c,
            );
        };
        // Most sequences take a count that defaults to 1, also when given
        // as 0.
        let count = param(params, 0, 1);
        let screen = &mut self.screen;
        match (intermediates, c) {
            ([], '@') => screen.ich(count),
            ([], 'A') => screen.cuu(count),
            ([], 'B' | 'e') => screen.cud(count),
            ([], 'C' | 'a') => screen.cuf(count),
            ([], 'D') => screen.cub(count),
            ([], 'E') => screen.cnl(count),
            ([], 'F') => screen.cpl(count),
            ([], 'G' | '`') => screen.cha(count),
            ([], 'H' | 'f') => screen.cup((count, param(params, 1, 1))),
            ([], 'I') => screen.cht(count),
            ([], 'Z') => screen.cbt(count),
            ([], 'b') => screen.rep(count),
            ([], 'g') => screen.tbc(param(params, 0, 0)),
            ([], 'h') => screen.sm(params, true, unhandled),
            ([], 'l') => screen.sm(params, false, unhandled),
            ([], 's') if no_params(params) => screen.decsc(),
            ([], 'u') if no_params(params) => screen.decrc(),
            ([] | [b'?', ..], 'J') => screen.ed(param(params, 0, 0), unhandled),
            ([] | [b'?', ..], 'K') => screen.el(param(params, 0, 0), unhandled),
            ([], 'L') => screen.il(count),
            ([], 'M') => screen.dl(count),
            ([], 'P') => screen.dch(count),
            ([], 'S') => screen.su(count),
            ([], 'T') => screen.sd(count),
            ([], 'X') => screen.ech(count),
            ([], 'd') => screen.vpa(count),
            ([], 'm') => screen.sgr(params, unhandled),
            ([], 'r') => {
                let rows = screen.grid().size().rows;
                screen.decstbm((count, param(params, 1, rows)));
            }
            ([], 't') if raw_param(params, 0) == Some(8) => {
                let (rows, cols) = screen.size();
                let size = (
                    raw_param(params, 1).unwrap_or(rows),
                    raw_param(params, 2).unwrap_or(cols),
                );
                self.callbacks.resize(screen, size);
            }
            ([b'?', ..], 'h') => screen.decset(params, unhandled),
            ([b'?', ..], 'l') => screen.decrst(params, unhandled),
            ([b'!'], 'p') => screen.decstr(),
            _ => unhandled(screen),
        }
    }

    fn hook(&mut self, _params: &vte::Params, intermediates: &[u8], ignore: bool, c: char) {
        self.dcs = (!ignore).then(|| (intermediates.to_vec(), c, Vec::new()));
    }

    fn put(&mut self, b: u8) {
        if let Some((_, _, data)) = &mut self.dcs {
            if data.len() < MAX_DCS_LEN {
                data.push(b);
            }
        }
    }

    fn unhook(&mut self) {
        if let Some((intermediates, c, data)) = self.dcs.take() {
            self.callbacks
                .unhandled_dcs(&mut self.screen, &intermediates, c, &data);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bel_terminated: bool) {
        match params {
            [b"0", s] => {
                self.callbacks.set_window_icon_name(&mut self.screen, s);
                self.callbacks.set_window_title(&mut self.screen, s);
            }
            [b"1", s] => {
                self.callbacks.set_window_icon_name(&mut self.screen, s);
            }
            [b"2", s] => {
                self.callbacks.set_window_title(&mut self.screen, s);
            }
            [b"52", ty, data] if ty.iter().all(|c| CLIPBOARD_SELECTOR.contains(c)) => {
                if *data == b"?" {
                    self.callbacks.paste_from_clipboard(&mut self.screen, ty);
                } else if data.iter().all(|c| BASE64.contains(c)) {
                    self.callbacks.copy_to_clipboard(&mut self.screen, ty, data);
                } else {
                    self.callbacks.unhandled_osc(&mut self.screen, params);
                }
            }
            _ => {
                self.callbacks.unhandled_osc(&mut self.screen, params);
            }
        }
    }
}

/// Whether a sequence carried no parameters at all (`CSI s`, not `CSI 1 s`).
fn no_params(params: &vte::Params) -> bool {
    params.iter().all(|param| param == [0]) && params.len() <= 1
}

/// The first value of parameter `index`, if there is one.
fn raw_param(params: &vte::Params, index: usize) -> Option<u16> {
    params
        .iter()
        .nth(index)
        .and_then(|param| param.first().copied())
}

/// The first value of parameter `index`, or `default` when it is missing
/// or 0.
fn param(params: &vte::Params, index: usize, default: u16) -> u16 {
    raw_param(params, index)
        .filter(|n| *n != 0)
        .unwrap_or(default)
}
