//! 全局热键：解析、录制、换绑。
//!
//! 三层都在这里：字符串 ↔ HotKey（配置里存的形式）、物理键码 ↔ 键名
//! （录制时用）、以及"先注册新的再注销旧的"这个换绑顺序。
//! 换绑顺序单独说一句：反过来的话新键注册失败时旧的已经没了，用户会
//! 陷入"热键彻底失效、只能改配置文件"的状态。

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::GlobalHotKeyManager;
use tao::keyboard::{KeyCode, ModifiersState};

/// 把 "ctrl+alt+s" 写成人看的 "Ctrl + Alt + S"。
pub(super) fn pretty_hotkey(spec: &str) -> String {
    if spec.trim().is_empty() {
        return "未设置".to_string();
    }
    spec.split('+')
        .map(|part| match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" | "option" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "cmd" | "super" | "win" | "meta" => "Cmd".to_string(),
            other => other.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) fn parse_hotkey(spec: &str) -> Option<HotKey> {
    let mut modifiers = Modifiers::empty();
    let mut code = None;
    for token in spec.to_lowercase().split('+') {
        match token.trim() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "cmd" | "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            "" => {}
            other => code = Some(code_of(other)?),
        }
    }
    code.map(|c| HotKey::new(Some(modifiers), c))
}

/// 键名 → 键码。覆盖字母、数字、F1–F12，够录制任意常用组合。
fn code_of(token: &str) -> Option<Code> {
    Some(match token {
        "space" => Code::Space,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        _ => return None,
    })
}

/// 录制时把物理键码转成规范的键名。
pub(super) fn key_token(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Space => "space",
        KeyCode::F1 => "f1",
        KeyCode::F2 => "f2",
        KeyCode::F3 => "f3",
        KeyCode::F4 => "f4",
        KeyCode::F5 => "f5",
        KeyCode::F6 => "f6",
        KeyCode::F7 => "f7",
        KeyCode::F8 => "f8",
        KeyCode::F9 => "f9",
        KeyCode::F10 => "f10",
        KeyCode::F11 => "f11",
        KeyCode::F12 => "f12",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::KeyA => "a",
        KeyCode::KeyB => "b",
        KeyCode::KeyC => "c",
        KeyCode::KeyD => "d",
        KeyCode::KeyE => "e",
        KeyCode::KeyF => "f",
        KeyCode::KeyG => "g",
        KeyCode::KeyH => "h",
        KeyCode::KeyI => "i",
        KeyCode::KeyJ => "j",
        KeyCode::KeyK => "k",
        KeyCode::KeyL => "l",
        KeyCode::KeyM => "m",
        KeyCode::KeyN => "n",
        KeyCode::KeyO => "o",
        KeyCode::KeyP => "p",
        KeyCode::KeyQ => "q",
        KeyCode::KeyR => "r",
        KeyCode::KeyS => "s",
        KeyCode::KeyT => "t",
        KeyCode::KeyU => "u",
        KeyCode::KeyV => "v",
        KeyCode::KeyW => "w",
        KeyCode::KeyX => "x",
        KeyCode::KeyY => "y",
        KeyCode::KeyZ => "z",
        _ => return None,
    })
}

/// 把当前按下的修饰键 + 主键拼成配置里的写法。
/// 必须带至少一个修饰键，否则会把普通打字也吃成热键。
pub(super) fn compose_spec(mods: ModifiersState, token: &str) -> Option<String> {
    let mut parts = Vec::new();
    if mods.control_key() {
        parts.push("ctrl");
    }
    if mods.alt_key() {
        parts.push("alt");
    }
    if mods.shift_key() {
        parts.push("shift");
    }
    if mods.super_key() {
        parts.push("cmd");
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(token);
    Some(parts.join("+"))
}

/// 换一个全局热键：先注册新的，成功再注销旧的；失败则把旧的装回去。
pub(super) fn apply_hotkey(
    manager: &mut Option<GlobalHotKeyManager>,
    current: &mut Option<HotKey>,
    hotkey_id: &mut Option<u32>,
    spec: &str,
) -> Result<(), String> {
    let Some(next) = parse_hotkey(spec) else {
        return Err(format!("不认识的组合键：{}", spec));
    };
    let Some(mgr) = manager.as_mut() else {
        return Err("热键管理器没有初始化".to_string());
    };
    if let Some(old) = current.as_ref() {
        if old.id() == next.id() {
            return Ok(());
        }
    }
    let previous = current.take();
    match mgr.register(next) {
        Ok(()) => {
            if let Some(old) = previous {
                let _ = mgr.unregister(old);
            }
            *current = Some(next);
            *hotkey_id = Some(next.id());
            Ok(())
        }
        Err(err) => {
            if let Some(old) = previous {
                let _ = mgr.register(old);
                *current = Some(old);
            }
            Err(format!("这个组合被系统或别的程序占用了：{}", err))
        }
    }
}
