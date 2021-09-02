// Copyright 2019-2021 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0

use raw_window_handle::RawWindowHandle;
use std::{collections::HashMap, ffi::CString, fmt, sync::Mutex};

use winapi::{
  shared::{basetsd, minwindef, windef},
  um::{commctrl, winuser},
};

use crate::{
  accelerator::Accelerator,
  event::{Event, WindowEvent},
  keyboard::{KeyCode, ModifiersState},
  menu::{CustomMenuItem, MenuId, MenuItem, MenuType},
  platform_impl::platform::util,
  window::WindowId as RootWindowId,
};

use super::{accelerator::register_accel, keyboard::key_to_vk, util::to_wstring, WindowId};

#[derive(Copy, Clone)]
struct AccelWrapper(winuser::ACCEL);
impl fmt::Debug for AccelWrapper {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
    f.pad(&format!(""))
  }
}

const CUT_ID: usize = 5001;
const COPY_ID: usize = 5002;
const PASTE_ID: usize = 5003;
const SELECT_ALL_ID: usize = 5004;
const HIDE_ID: usize = 5005;
const CLOSE_ID: usize = 5006;
const QUIT_ID: usize = 5007;
const MINIMIZE_ID: usize = 5008;

lazy_static! {
  static ref MENU_IDS: Mutex<Vec<u16>> = Mutex::new(vec![]);
}

pub struct MenuHandler {
  window_id: Option<RootWindowId>,
  menu_type: MenuType,
  event_sender: Box<dyn Fn(Event<'static, ()>)>,
}

impl MenuHandler {
  pub fn new(
    event_sender: Box<dyn Fn(Event<'static, ()>)>,
    menu_type: MenuType,
    window_id: Option<RootWindowId>,
  ) -> MenuHandler {
    MenuHandler {
      window_id,
      menu_type,
      event_sender,
    }
  }
  pub fn send_menu_event(&self, menu_id: u16) {
    (self.event_sender)(Event::MenuEvent {
      menu_id: MenuId(menu_id),
      origin: self.menu_type,
      window_id: self.window_id,
    });
  }

  pub fn send_event(&self, event: Event<'static, ()>) {
    (self.event_sender)(event);
  }
}

#[derive(Debug, Clone)]
pub struct MenuItemAttributes(pub(crate) u16, windef::HMENU);

impl MenuItemAttributes {
  pub fn id(&self) -> MenuId {
    MenuId(self.0)
  }
  pub fn set_enabled(&mut self, enabled: bool) {
    unsafe {
      winuser::EnableMenuItem(
        self.1,
        self.0 as u32,
        match enabled {
          true => winuser::MF_ENABLED,
          false => winuser::MF_DISABLED,
        },
      );
    }
  }
  pub fn set_title(&mut self, title: &str) {
    unsafe {
      let mut info = winuser::MENUITEMINFOA {
        cbSize: std::mem::size_of::<winuser::MENUITEMINFOA>() as _,
        fMask: winuser::MIIM_STRING,
        ..Default::default()
      };
      let c_str = CString::new(title).unwrap();
      info.dwTypeData = c_str.as_ptr() as _;

      winuser::SetMenuItemInfoA(self.1, self.0 as u32, minwindef::FALSE, &info);
    }
  }
  pub fn set_selected(&mut self, selected: bool) {
    unsafe {
      winuser::CheckMenuItem(
        self.1,
        self.0 as u32,
        match selected {
          true => winuser::MF_CHECKED,
          false => winuser::MF_UNCHECKED,
        },
      );
    }
  }

  // todo: set custom icon to the menu item
  pub fn set_icon(&self, icon: Vec<u8>) {
    if let Some(hicon) = super::util::get_hicon_from_buffer(&icon[..], 32, 32) {
      unsafe {
        if let Some(hbitmap) = util::get_hbitmap_from_hicon(hicon, 16, 16) {
          let info = winuser::MENUITEMINFOA {
            cbSize: std::mem::size_of::<winuser::MENUITEMINFOA>() as _,
            fMask: winuser::MIIM_BITMAP,
            hbmpItem: hbitmap,
            ..Default::default()
          };
          winuser::SetMenuItemInfoA(self.1, self.0 as u32, minwindef::FALSE, &info);
        }
      }
    };
  }
}

#[derive(Debug, Clone)]
pub struct Menu {
  hmenu: windef::HMENU,
  accels: HashMap<u16, AccelWrapper>,
}

unsafe impl Send for Menu {}
unsafe impl Sync for Menu {}

impl Default for Menu {
  fn default() -> Self {
    Menu::new()
  }
}

impl Menu {
  pub fn new() -> Self {
    unsafe {
      let hmenu = winuser::CreateMenu();
      Menu {
        hmenu,
        accels: HashMap::default(),
      }
    }
  }

  pub fn new_popup_menu() -> Self {
    unsafe {
      let hmenu = winuser::CreatePopupMenu();
      Menu {
        hmenu,
        accels: HashMap::default(),
      }
    }
  }

  pub fn hmenu(&self) -> windef::HMENU {
    self.hmenu
  }

  // Get the accels table
  pub(crate) fn accels(&self) -> Option<Vec<winuser::ACCEL>> {
    if self.accels.is_empty() {
      return None;
    }
    Some(self.accels.values().cloned().map(|d| d.0).collect())
  }

  pub fn add_item(
    &mut self,
    menu_id: MenuId,
    title: &str,
    accelerators: Option<Accelerator>,
    enabled: bool,
    selected: bool,
    _menu_type: MenuType,
  ) -> CustomMenuItem {
    unsafe {
      let mut flags = winuser::MF_STRING;
      if !enabled {
        flags |= winuser::MF_GRAYED;
      }
      if selected {
        flags |= winuser::MF_CHECKED;
      }

      let mut anno_title = title.to_string();
      // format title
      if let Some(accelerators) = accelerators.clone() {
        anno_title.push('\t');
        format_hotkey(accelerators, &mut anno_title);
      }

      winuser::AppendMenuW(
        self.hmenu,
        flags,
        menu_id.0 as _,
        to_wstring(&anno_title).as_mut_ptr(),
      );

      // add our accels
      if let Some(accelerators) = accelerators {
        if let Some(accelerators) = convert_accelerator(menu_id.0, accelerators) {
          self.accels.insert(menu_id.0, AccelWrapper(accelerators));
        }
      }
      MENU_IDS.lock().unwrap().push(menu_id.0 as _);
      CustomMenuItem(MenuItemAttributes(menu_id.0, self.hmenu))
    }
  }

  pub fn add_submenu(&mut self, title: &str, enabled: bool, mut submenu: Menu) {
    unsafe {
      let child_accels = std::mem::take(&mut submenu.accels);
      self.accels.extend(child_accels);

      let mut flags = winuser::MF_POPUP;
      if !enabled {
        flags |= winuser::MF_DISABLED;
      }

      winuser::AppendMenuW(
        self.hmenu,
        flags,
        submenu.hmenu() as _,
        to_wstring(&title).as_mut_ptr(),
      );
    }
  }

  pub fn add_native_item(
    &mut self,
    item: MenuItem,
    _menu_type: MenuType,
  ) -> Option<CustomMenuItem> {
    match item {
      MenuItem::Separator => {
        unsafe {
          winuser::AppendMenuW(self.hmenu, winuser::MF_SEPARATOR, 0, std::ptr::null());
        };
      }
      MenuItem::Cut => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          CUT_ID,
          to_wstring("&Cut\tCtrl+X").as_mut_ptr(),
        );
      },
      MenuItem::Copy => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          COPY_ID,
          to_wstring("&Copy\tCtrl+C").as_mut_ptr(),
        );
      },
      MenuItem::Paste => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          PASTE_ID,
          to_wstring("&Paste\tCtrl+V").as_mut_ptr(),
        );
      },
      MenuItem::SelectAll => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          SELECT_ALL_ID,
          to_wstring("&Select all\tCtrl+A").as_mut_ptr(),
        );
      },
      MenuItem::Hide => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          HIDE_ID,
          to_wstring("&Hide\tCtrl+H").as_mut_ptr(),
        );
      },
      MenuItem::CloseWindow => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          CLOSE_ID,
          to_wstring("&Close\tAlt+F4").as_mut_ptr(),
        );
      },
      MenuItem::Quit => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          QUIT_ID,
          to_wstring("&Quit").as_mut_ptr(),
        );
      },
      MenuItem::Minimize => unsafe {
        winuser::AppendMenuW(
          self.hmenu,
          winuser::MF_STRING,
          MINIMIZE_ID,
          to_wstring("&Minimize").as_mut_ptr(),
        );
      },
      // FIXME: create all shortcuts of MenuItem if possible...
      // like linux?
      _ => (),
    };

    None
  }
}

/*
  Disabled as menu's seems to be linked to the app
  so they are dropped when the app closes.
  see discussion here;

  https://github.com/tauri-apps/tao/pull/106#issuecomment-880034210

  impl Drop for Menu {
    fn drop(&mut self) {
      unsafe {
        winuser::DestroyMenu(self.hmenu);
      }
    }
  }
*/

const MENU_SUBCLASS_ID: usize = 4568;

pub fn initialize(
  menu_builder: Menu,
  window_handle: RawWindowHandle,
  menu_handler: MenuHandler,
) -> Option<windef::HMENU> {
  if let RawWindowHandle::Windows(handle) = window_handle {
    let sender: *mut MenuHandler = Box::into_raw(Box::new(menu_handler));
    let menu = menu_builder.clone().hmenu();

    unsafe {
      commctrl::SetWindowSubclass(
        handle.hwnd as _,
        Some(subclass_proc),
        MENU_SUBCLASS_ID,
        sender as _,
      );
      winuser::SetMenu(handle.hwnd as _, menu);
    }

    if let Some(accels) = menu_builder.accels() {
      register_accel(handle.hwnd as _, &accels);
    }

    Some(menu)
  } else {
    None
  }
}

pub(crate) unsafe extern "system" fn subclass_proc(
  hwnd: windef::HWND,
  msg: minwindef::UINT,
  wparam: minwindef::WPARAM,
  lparam: minwindef::LPARAM,
  _id: basetsd::UINT_PTR,
  subclass_input_ptr: basetsd::DWORD_PTR,
) -> minwindef::LRESULT {
  let subclass_input_ptr = subclass_input_ptr as *mut MenuHandler;
  let subclass_input = &*(subclass_input_ptr);

  if msg == winuser::WM_DESTROY {
    Box::from_raw(subclass_input_ptr);
  }

  match msg {
    winuser::WM_COMMAND => {
      match wparam {
        CUT_ID => {
          execute_edit_command(EditCommand::Cut);
        }
        COPY_ID => {
          execute_edit_command(EditCommand::Copy);
        }
        PASTE_ID => {
          execute_edit_command(EditCommand::Paste);
        }
        SELECT_ALL_ID => {
          execute_edit_command(EditCommand::SelectAll);
        }
        HIDE_ID => {
          winuser::ShowWindow(hwnd, winuser::SW_HIDE);
        }
        CLOSE_ID => {
          subclass_input.send_event(Event::WindowEvent {
            window_id: RootWindowId(WindowId(hwnd)),
            event: WindowEvent::CloseRequested,
          });
        }
        QUIT_ID => {
          subclass_input.send_event(Event::LoopDestroyed);
        }
        MINIMIZE_ID => {
          winuser::ShowWindow(hwnd, winuser::SW_MINIMIZE);
        }
        _ => {
          let menu_id = minwindef::LOWORD(wparam as _);
          if MENU_IDS.lock().unwrap().contains(&menu_id) {
            subclass_input.send_menu_event(menu_id);
          }
        }
      }
      0
    }
    _ => commctrl::DefSubclassProc(hwnd, msg, wparam, lparam),
  }
}

enum EditCommand {
  Copy,
  Cut,
  Paste,
  SelectAll,
}
fn execute_edit_command(command: EditCommand) {
  let key = match command {
    EditCommand::Copy => 0x43,      // c
    EditCommand::Cut => 0x58,       // x
    EditCommand::Paste => 0x56,     // v
    EditCommand::SelectAll => 0x41, // a
  };

  unsafe {
    let mut inputs: [winuser::INPUT; 4] = std::mem::zeroed();
    inputs[0].type_ = winuser::INPUT_KEYBOARD;
    inputs[0].u.ki_mut().wVk = winuser::VK_CONTROL as _;

    inputs[1].type_ = winuser::INPUT_KEYBOARD;
    inputs[1].u.ki_mut().wVk = key;

    inputs[2].type_ = winuser::INPUT_KEYBOARD;
    inputs[2].u.ki_mut().wVk = key;
    inputs[2].u.ki_mut().dwFlags = winuser::KEYEVENTF_KEYUP;

    inputs[3].type_ = winuser::INPUT_KEYBOARD;
    inputs[3].u.ki_mut().wVk = winuser::VK_CONTROL as _;
    inputs[3].u.ki_mut().dwFlags = winuser::KEYEVENTF_KEYUP;

    winuser::SendInput(
      inputs.len() as _,
      inputs.as_mut_ptr(),
      std::mem::size_of::<winuser::INPUT>() as _,
    );
  }
}

// Convert a hotkey to an accelerator.
fn convert_accelerator(id: u16, key: Accelerator) -> Option<winuser::ACCEL> {
  let mut virt_key = winuser::FVIRTKEY;
  let key_mods: ModifiersState = key.mods;
  if key_mods.control_key() {
    virt_key |= winuser::FCONTROL;
  }
  if key_mods.alt_key() {
    virt_key |= winuser::FALT;
  }
  if key_mods.shift_key() {
    virt_key |= winuser::FSHIFT;
  }

  let raw_key = if let Some(vk_code) = key_to_vk(&key.key) {
    let mod_code = vk_code >> 8;
    if mod_code & 0x1 != 0 {
      virt_key |= winuser::FSHIFT;
    }
    if mod_code & 0x02 != 0 {
      virt_key |= winuser::FCONTROL;
    }
    if mod_code & 0x04 != 0 {
      virt_key |= winuser::FALT;
    }
    vk_code & 0x00ff
  } else {
    dbg!("Failed to convert key {:?} into virtual key code", key.key);
    return None;
  };

  Some(winuser::ACCEL {
    fVirt: virt_key,
    key: raw_key as u16,
    cmd: id,
  })
}

// Format the hotkey in a Windows-native way.
fn format_hotkey(key: Accelerator, s: &mut String) {
  let key_mods: ModifiersState = key.mods;
  if key_mods.control_key() {
    s.push_str("Ctrl+");
  }
  if key_mods.shift_key() {
    s.push_str("Shift+");
  }
  if key_mods.alt_key() {
    s.push_str("Alt+");
  }
  if key_mods.super_key() {
    s.push_str("Windows+");
  }
  match &key.key {
    KeyCode::KeyA => s.push('A'),
    KeyCode::KeyB => s.push('B'),
    KeyCode::KeyC => s.push('C'),
    KeyCode::KeyD => s.push('D'),
    KeyCode::KeyE => s.push('E'),
    KeyCode::KeyF => s.push('F'),
    KeyCode::KeyG => s.push('G'),
    KeyCode::KeyH => s.push('H'),
    KeyCode::KeyI => s.push('I'),
    KeyCode::KeyJ => s.push('J'),
    KeyCode::KeyK => s.push('K'),
    KeyCode::KeyL => s.push('L'),
    KeyCode::KeyM => s.push('M'),
    KeyCode::KeyN => s.push('N'),
    KeyCode::KeyO => s.push('O'),
    KeyCode::KeyP => s.push('P'),
    KeyCode::KeyQ => s.push('Q'),
    KeyCode::KeyR => s.push('R'),
    KeyCode::KeyS => s.push('S'),
    KeyCode::KeyT => s.push('T'),
    KeyCode::KeyU => s.push('U'),
    KeyCode::KeyV => s.push('V'),
    KeyCode::KeyW => s.push('W'),
    KeyCode::KeyX => s.push('X'),
    KeyCode::KeyY => s.push('Y'),
    KeyCode::KeyZ => s.push('Z'),
    KeyCode::Digit0 => s.push('0'),
    KeyCode::Digit1 => s.push('1'),
    KeyCode::Digit2 => s.push('2'),
    KeyCode::Digit3 => s.push('3'),
    KeyCode::Digit4 => s.push('4'),
    KeyCode::Digit5 => s.push('5'),
    KeyCode::Digit6 => s.push('6'),
    KeyCode::Digit7 => s.push('7'),
    KeyCode::Digit8 => s.push('8'),
    KeyCode::Digit9 => s.push('9'),
    KeyCode::Escape => s.push_str("Esc"),
    KeyCode::Delete => s.push_str("Del"),
    KeyCode::Insert => s.push_str("Ins"),
    KeyCode::PageUp => s.push_str("PgUp"),
    KeyCode::PageDown => s.push_str("PgDn"),
    // These names match LibreOffice.
    KeyCode::ArrowLeft => s.push_str("Left"),
    KeyCode::ArrowRight => s.push_str("Right"),
    KeyCode::ArrowUp => s.push_str("Up"),
    KeyCode::ArrowDown => s.push_str("Down"),
    _ => s.push_str(&format!("{:?}", key.key)),
  }
}
