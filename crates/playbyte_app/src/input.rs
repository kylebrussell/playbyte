use gilrs::Button;
use winit::keyboard::KeyCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    NextItem,
    PrevItem,
    ToggleOverlay,
    CreateByte,
    BeginRenameCurrent,
    OpenOfficialPickerCurrent,
    CancelUi,
    OfficialPickerMove(i32),
    OfficialPickerConfirm,
    SelectIndex(usize),
    RenameTitle { index: usize, title: String },
    SetOfficialTitle { index: usize, title: String },
    ClearOfficialTitle { index: usize },
}

#[derive(Debug, Clone)]
pub enum UserEvent {
    Action(Action),
    GamepadButton { button: Button, pressed: bool },
}

#[derive(Debug, Clone, Copy)]
pub struct ButtonContext {
    pub overlay_visible: bool,
    pub official_picker_open: bool,
    pub is_editing_text: bool,
}

impl ButtonContext {
    pub fn capture_gameplay(self) -> bool {
        self.overlay_visible || self.official_picker_open || self.is_editing_text
    }
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

pub fn action_from_button(
    button: Button,
    pressed: bool,
    context: ButtonContext,
) -> Option<Action> {
    if !pressed {
        return None;
    }
    if context.official_picker_open {
        return match button {
            Button::DPadUp => Some(Action::OfficialPickerMove(-1)),
            Button::DPadDown => Some(Action::OfficialPickerMove(1)),
            Button::South => Some(Action::OfficialPickerConfirm),
            Button::East | Button::Start => Some(Action::CancelUi),
            _ => None,
        };
    }

    if context.overlay_visible {
        return match button {
            Button::DPadLeft | Button::LeftTrigger | Button::LeftTrigger2 => Some(Action::PrevItem),
            Button::DPadRight | Button::RightTrigger | Button::RightTrigger2 => {
                Some(Action::NextItem)
            }
            Button::Start => Some(Action::ToggleOverlay),
            Button::South => Some(Action::ToggleOverlay),
            Button::North => Some(Action::OpenOfficialPickerCurrent),
            Button::West => Some(Action::CreateByte),
            Button::Select => Some(Action::BeginRenameCurrent),
            Button::East => Some(Action::CancelUi),
            _ => None,
        };
    }

    match button {
        Button::Start => Some(Action::ToggleOverlay),
        _ => None,
    }
}
