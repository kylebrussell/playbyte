use gilrs::Button;
use winit::keyboard::KeyCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    NextItem,
    PrevItem,
    ToggleOverlay,
    CreateByte,
    SelectIndex(usize),
    RenameTitle { index: usize, title: String },
    SetOfficialTitle { index: usize, title: String },
    ClearOfficialTitle { index: usize },
}

#[derive(Debug, Clone)]
pub enum UserEvent {
    Action(Action),
}

pub fn action_from_key(key: KeyCode, pressed: bool) -> Option<Action> {
    if !pressed {
        return None;
    }
    match key {
        KeyCode::PageUp => Some(Action::PrevItem),
        KeyCode::PageDown => Some(Action::NextItem),
        KeyCode::Tab => Some(Action::ToggleOverlay),
        KeyCode::KeyB => Some(Action::CreateByte),
        _ => None,
    }
}

pub fn action_from_button(button: Button, pressed: bool) -> Option<Action> {
    if !pressed {
        return None;
    }
    match button {
        Button::LeftTrigger2 => Some(Action::PrevItem),
        Button::RightTrigger2 => Some(Action::NextItem),
        _ => None,
    }
}
