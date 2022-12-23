use config::keyassignment::*;
use config::{ConfigHandle, DeferredKeyCode};
use mux::domain::DomainState;
use mux::Mux;
use ordered_float::NotNan;
use std::borrow::Cow;
use std::convert::TryFrom;
use window::{KeyCode, Modifiers};
use KeyAssignment::*;

/// Describes an argument/parameter/context that is required
/// in order for the command to have meaning.
/// The intent is for this to be used when filtering the items
/// that should be shown in eg: a context menu.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ArgType {
    /// Operates on the active pane
    ActivePane,
    /// Operates on the active tab
    ActiveTab,
    /// Operates on the active window
    ActiveWindow,
}

/// A helper function used to synthesize key binding permutations.
/// If the input is a character on a US ANSI keyboard layout, returns
/// the the typical character that is produced when holding down
/// the shift key and pressing the original key.
/// This doesn't produce an exhaustive list because there are only
/// a handful of default assignments in the command DEFS below.
fn us_layout_shift(s: &str) -> String {
    match s {
        "1" => "!".to_string(),
        "2" => "@".to_string(),
        "3" => "#".to_string(),
        "4" => "$".to_string(),
        "5" => "%".to_string(),
        "6" => "^".to_string(),
        "7" => "&".to_string(),
        "8" => "*".to_string(),
        "9" => "(".to_string(),
        "0" => ")".to_string(),
        "[" => "{".to_string(),
        "]" => "}".to_string(),
        "=" => "+".to_string(),
        "-" => "_".to_string(),
        "'" => "\"".to_string(),
        s if s.len() == 1 => s.to_ascii_uppercase(),
        s => s.to_string(),
    }
}

/// `CommandDef` defines a command in the UI.
pub struct CommandDef {
    /// Brief description
    pub brief: Cow<'static, str>,
    /// A longer, more detailed, description
    pub doc: Cow<'static, str>,
    /// The key assignments associated with this command.
    pub keys: Vec<(Modifiers, String)>,
    /// The argument types/context in which this command is valid.
    pub args: &'static [ArgType],
    /// Where to place the command in a menubar
    pub menubar: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct ExpandedCommand {
    pub brief: Cow<'static, str>,
    pub doc: Cow<'static, str>,
    pub action: KeyAssignment,
    pub keys: Vec<(Modifiers, KeyCode)>,
    pub menubar: &'static [&'static str],
}

impl std::fmt::Debug for CommandDef {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("CommandDef")
            .field("brief", &self.brief)
            .field("doc", &self.doc)
            .field("keys", &self.keys)
            .field("args", &self.args)
            .finish()
    }
}

impl CommandDef {
    /// Blech. Depending on the OS, a shifted key combination
    /// such as CTRL-SHIFT-L may present as either:
    /// CTRL+SHIFT + mapped lowercase l
    /// CTRL+SHIFT + mapped uppercase l
    /// CTRL       + mapped uppercase l
    ///
    /// This logic synthesizes the different combinations so
    /// that it isn't such a headache to maintain the mapping
    /// and prevents missing cases.
    ///
    /// Note that the mapped form of these things assumes
    /// US layout for some of the special shifted/punctuation cases.
    /// It's not perfect.
    ///
    /// The synthesis here requires that the defaults in
    /// the keymap below use the lowercase form of single characters!
    fn permute_keys(&self, config: &ConfigHandle) -> Vec<(Modifiers, KeyCode)> {
        let mut keys = vec![];

        for (mods, label) in &self.keys {
            let mods = *mods;
            let key = DeferredKeyCode::try_from(label.as_str())
                .unwrap()
                .resolve(config.key_map_preference)
                .clone();

            let ukey = DeferredKeyCode::try_from(us_layout_shift(&label))
                .unwrap()
                .resolve(config.key_map_preference)
                .clone();

            keys.push((mods, key.clone()));

            if mods == Modifiers::SUPER {
                // We want each SUPER/CMD version of the keys to also have
                // CTRL+SHIFT version(s) for environments where SUPER/CMD
                // is reserved for the window manager.
                // This bit synthesizes those.
                keys.push((Modifiers::CTRL | Modifiers::SHIFT, key.clone()));
                if ukey != key {
                    keys.push((Modifiers::CTRL | Modifiers::SHIFT, ukey.clone()));
                    keys.push((Modifiers::CTRL, ukey.clone()));
                }
            } else if mods.contains(Modifiers::SHIFT) && ukey != key {
                keys.push((mods, ukey.clone()));
                keys.push((mods - Modifiers::SHIFT, ukey.clone()));
            }
        }

        keys
    }

    /// Produces the list of default key assignments and actions.
    /// Used by the InputMap.
    pub fn default_key_assignments(
        config: &ConfigHandle,
    ) -> Vec<(Modifiers, KeyCode, KeyAssignment)> {
        let mut result = vec![];
        for cmd in Self::expanded_commands(config) {
            for (mods, code) in cmd.keys {
                result.push((mods, code.clone(), cmd.action.clone()));
            }
        }
        result
    }

    /// Produces the complete set of expanded commands.
    pub fn expanded_commands(config: &ConfigHandle) -> Vec<ExpandedCommand> {
        let mut result = vec![];

        for action in compute_default_actions() {
            match derive_command_from_key_assignment(&action) {
                None => log::warn!(
                    "{action:?} is a default action, but we cannot derive a CommandDef for it"
                ),
                Some(def) => {
                    let keys = def.permute_keys(config);
                    result.push(ExpandedCommand {
                        brief: def.brief.into(),
                        doc: def.doc.into(),
                        keys,
                        action,
                        menubar: def.menubar,
                    });
                }
            }
        }

        // Generate some stuff based on the config
        for cmd in &config.launch_menu {
            let label = match cmd.label.as_ref() {
                Some(label) => label.to_string(),
                None => match cmd.args.as_ref() {
                    Some(args) => args.join(" "),
                    None => "(default shell)".to_string(),
                },
            };
            result.push(ExpandedCommand {
                brief: format!("{label} (New Tab)").into(),
                doc: "".into(),
                keys: vec![],
                action: KeyAssignment::SpawnCommandInNewTab(cmd.clone()),
                menubar: &["Shell"],
            });
        }

        // Generate some stuff based on the mux state
        if let Some(mux) = Mux::try_get() {
            let mut domains = mux.iter_domains();
            domains.sort_by(|a, b| {
                let a_state = a.state();
                let b_state = b.state();
                if a_state != b_state {
                    use std::cmp::Ordering;
                    return if a_state == DomainState::Attached {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                a.domain_id().cmp(&b.domain_id())
            });
            for dom in &domains {
                let name = dom.domain_name();
                // FIXME: use domain_label here, but needs to be async
                let label = name.clone();

                if dom.spawnable() {
                    if dom.state() == DomainState::Attached {
                        result.push(ExpandedCommand {
                            brief: format!("New Tab (Domain {label})").into(),
                            doc: "".into(),
                            keys: vec![],
                            action: KeyAssignment::SpawnCommandInNewTab(SpawnCommand {
                                domain: SpawnTabDomain::DomainName(name.to_string()),
                                ..SpawnCommand::default()
                            }),
                            menubar: &["Shell"],
                        });
                    } else {
                        result.push(ExpandedCommand {
                            brief: format!("Attach Domain {label}").into(),
                            doc: "".into(),
                            keys: vec![],
                            action: KeyAssignment::AttachDomain(name.to_string()),
                            menubar: &["Shell"],
                        });
                    }
                }
            }
            for dom in &domains {
                let name = dom.domain_name();
                // FIXME: use domain_label here, but needs to be async
                let label = name.clone();

                if dom.state() == DomainState::Attached {
                    if name == "local" {
                        continue;
                    }
                    result.push(ExpandedCommand {
                        brief: format!("Detach Domain {label}").into(),
                        doc: "".into(),
                        keys: vec![],
                        action: KeyAssignment::DetachDomain(SpawnTabDomain::DomainName(
                            name.to_string(),
                        )),
                        menubar: &["Shell", "Detach"],
                    });
                }
            }

            let active_workspace = mux.active_workspace();
            for workspace in mux.iter_workspaces() {
                if workspace != active_workspace {
                    result.push(ExpandedCommand {
                        brief: format!("Switch to workspace {workspace}").into(),
                        doc: "".into(),
                        keys: vec![],
                        action: KeyAssignment::SwitchToWorkspace {
                            name: Some(workspace.clone()),
                            spawn: None,
                        },
                        menubar: &["Window", "Workspace"],
                    });
                }
            }
            result.push(ExpandedCommand {
                brief: "Create new Workspace".into(),
                doc: "".into(),
                keys: vec![],
                action: KeyAssignment::SwitchToWorkspace {
                    name: None,
                    spawn: None,
                },
                menubar: &["Window", "Workspace"],
            });
        }

        result
    }

    #[cfg(not(target_os = "macos"))]
    pub fn recreate_menubar(_config: &ConfigHandle) {}

    #[cfg(target_os = "macos")]
    pub fn recreate_menubar(config: &ConfigHandle) {
        use window::os::macos::menu::*;

        let main_menu = Menu::new_with_title("MainMenu");
        main_menu.assign_as_main_menu();

        let mut commands = Self::expanded_commands(config);
        commands.retain(|cmd| !cmd.menubar.is_empty());

        // Prefer to put the menus in this order
        let mut order: Vec<&'static str> = vec!["WezTerm", "Shell", "Edit", "View", "Window"];
        // Add any other menus on the end
        for cmd in &commands {
            if !order.contains(&cmd.menubar[0]) {
                order.push(cmd.menubar[0]);
            }
        }

        for &title in &order {
            for cmd in &commands {
                if cmd.menubar[0] != title {
                    continue;
                }

                let mut submenu = main_menu.get_or_create_sub_menu(&cmd.menubar[0], |menu| {
                    if cmd.menubar[0] == "Window" {
                        menu.assign_as_windows_menu();
                        // macOS will insert stuff at the top and bottom, so we add
                        // a separator to tidy things up a bit
                        menu.add_item(&MenuItem::new_separator());
                    } else if cmd.menubar[0] == "WezTerm" {
                        menu.assign_as_app_menu();

                        menu.add_item(&MenuItem::new_with(
                            "About WezTerm",
                            Some(sel!(weztermShowAbout:)),
                            "",
                        ));
                        menu.add_item(&MenuItem::new_separator());

                        // FIXME: when we set this as the services menu,
                        // both Help and trying to open Services cause
                        // the process to spin forever in some internal
                        // menu validation phase.
                        if false {
                            let services_menu = Menu::new_with_title("Services");
                            services_menu.assign_as_services_menu();
                            let services_item = MenuItem::new_with("Services", None, "");
                            menu.add_item(&services_item);
                            services_item.set_sub_menu(&services_menu);

                            menu.add_item(&MenuItem::new_separator());
                        }
                    } else if cmd.menubar[0] == "Help" {
                        menu.assign_as_help_menu();
                    }
                });

                // Fill out any submenu hierarchy
                for sub_title in cmd.menubar.iter().skip(1) {
                    submenu = submenu.get_or_create_sub_menu(sub_title, |_menu| {});
                }

                // And add the current command to the menu
                let item =
                    MenuItem::new_with(&cmd.brief, Some(sel!(weztermPerformKeyAssignment:)), "");

                item.set_represented_item(RepresentedItem::KeyAssignment(cmd.action.clone()));
                item.set_tool_tip(&cmd.doc);
                submenu.add_item(&item);
            }
        }
    }
}

/// Given "1" return "1st", "2" -> "2nd" and so on
fn english_ordinal(n: isize) -> String {
    let n = n.to_string();
    if n.ends_with('1') && !n.ends_with("11") {
        format!("{n}st")
    } else if n.ends_with('2') && !n.ends_with("12") {
        format!("{n}nd")
    } else if n.ends_with('3') && !n.ends_with("13") {
        format!("{n}rd")
    } else {
        format!("{n}th")
    }
}

/// Describes a key assignment action; returns a bunch
/// of metadata that is useful in the command palette/menubar context.
/// This function will be called for the result of compute_default_actions(),
/// but can also be used to describe user-provided commands
pub fn derive_command_from_key_assignment(action: &KeyAssignment) -> Option<CommandDef> {
    Some(match action {
        PasteFrom(ClipboardPasteSource::PrimarySelection) => CommandDef {
            brief: "Paste primary selection".into(),
            doc: "Pastes text from the primary selection".into(),
            keys: vec![(Modifiers::SHIFT, "Insert".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        CopyTo(ClipboardCopyDestination::PrimarySelection) => CommandDef {
            brief: "Copy to primary selection".into(),
            doc: "Copies text to the primary selection".into(),
            keys: vec![(Modifiers::CTRL, "Insert".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        CopyTo(ClipboardCopyDestination::Clipboard) => CommandDef {
            brief: "Copy to clipboard".into(),
            doc: "Copies text to the clipboard".into(),
            keys: vec![
                (Modifiers::SUPER, "c".into()),
                (Modifiers::NONE, "Copy".into()),
            ],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        CopyTo(ClipboardCopyDestination::ClipboardAndPrimarySelection) => CommandDef {
            brief: "Copy to clipboard and primary selection".into(),
            doc: "Copies text to the clipboard and the primary selection".into(),
            keys: vec![(Modifiers::CTRL, "Insert".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        PasteFrom(ClipboardPasteSource::Clipboard) => CommandDef {
            brief: "Paste from clipboard".into(),
            doc: "Pastes text from the clipboard".into(),
            keys: vec![
                (Modifiers::SUPER, "v".into()),
                (Modifiers::NONE, "Paste".into()),
            ],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        ToggleFullScreen => CommandDef {
            brief: "Toggle full screen mode".into(),
            doc: "Switch between normal and full screen mode".into(),
            keys: vec![(Modifiers::ALT, "Return".into())],
            args: &[ArgType::ActiveWindow],
            menubar: &["View"],
        },
        Hide => CommandDef {
            brief: "Hide/Minimize Window".into(),
            doc: "Hides/Mimimizes the current window".into(),
            keys: vec![(Modifiers::SUPER, "m".into())],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window"],
        },
        Show => CommandDef {
            brief: "Show/Restore Window".into(),
            doc: "Show/Restore the current window".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        HideApplication => CommandDef {
            brief: "Hide Application".into(),
            doc: "Hides all of the windows of the application. \
              This is macOS specific."
                .into(),
            keys: vec![(Modifiers::SUPER, "h".into())],
            args: &[],
            menubar: &["WezTerm"],
        },
        SpawnWindow => CommandDef {
            brief: "New Window".into(),
            doc: "Launches the default program into a new window".into(),
            keys: vec![(Modifiers::SUPER, "n".into())],
            args: &[],
            menubar: &["Shell"],
        },
        ClearScrollback(ScrollbackEraseMode::ScrollbackOnly) => CommandDef {
            brief: "Clear scrollback".into(),
            doc: "Clears any text that has scrolled out of the \
              viewport of the current pane"
                .into(),
            keys: vec![(Modifiers::SUPER, "k".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        ClearScrollback(ScrollbackEraseMode::ScrollbackAndViewport) => CommandDef {
            brief: "Clear the scrollback and viewport".into(),
            doc: "Removes all content from the screen and scrollback".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        Search(Pattern::CurrentSelectionOrEmptyString) => CommandDef {
            brief: "Search pane output".into(),
            doc: "Enters the search mode UI for the current pane".into(),
            keys: vec![(Modifiers::SUPER, "f".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        Search(_) => CommandDef {
            brief: "Search pane output".into(),
            doc: "Enters the search mode UI for the current pane".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        ShowDebugOverlay => CommandDef {
            brief: "Show debug overlay".into(),
            doc: "Activates the debug overlay and Lua REPL".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "l".into())],
            args: &[ArgType::ActiveWindow],
            menubar: &["Help"],
        },
        QuickSelect => CommandDef {
            brief: "Enter QuickSelect mode".into(),
            doc: "Activates the quick selection UI for the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "Space".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        QuickSelectArgs(_) => CommandDef {
            brief: "Enter QuickSelect mode".into(),
            doc: "Activates the quick selection UI for the current pane".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        CharSelect(_) => CommandDef {
            brief: "Enter Emoji / Character selection mode".into(),
            doc: "Activates the character selection UI for the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "u".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        PaneSelect(_) => CommandDef {
            brief: "Enter Pane selection mode".into(),
            doc: "Activates the pane selection UI".into(),
            keys: vec![], // FIXME: find a new assignment
            args: &[ArgType::ActivePane],
            menubar: &["Window"],
        },
        DecreaseFontSize => CommandDef {
            brief: "Decrease font size".into(),
            doc: "Scales the font size smaller by 10%".into(),
            keys: vec![
                (Modifiers::SUPER, "-".into()),
                (Modifiers::CTRL, "-".into()),
            ],
            args: &[ArgType::ActiveWindow],
            menubar: &["View", "Font Size"],
        },
        IncreaseFontSize => CommandDef {
            brief: "Increase font size".into(),
            doc: "Scales the font size larger by 10%".into(),
            keys: vec![
                (Modifiers::SUPER, "=".into()),
                (Modifiers::CTRL, "=".into()),
            ],
            args: &[ArgType::ActiveWindow],
            menubar: &["View", "Font Size"],
        },
        ResetFontSize => CommandDef {
            brief: "Reset font size".into(),
            doc: "Restores the font size to match your configuration file".into(),
            keys: vec![
                (Modifiers::SUPER, "0".into()),
                (Modifiers::CTRL, "0".into()),
            ],
            args: &[ArgType::ActiveWindow],
            menubar: &["View", "Font Size"],
        },
        ResetFontAndWindowSize => CommandDef {
            brief: "Reset the window and font size".into(),
            doc: "Restores the original window and font size".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["View", "Font Size"],
        },
        SpawnTab(SpawnTabDomain::CurrentPaneDomain) => CommandDef {
            brief: "New Tab".into(),
            doc: "Create a new tab in the same domain as the current pane".into(),
            keys: vec![(Modifiers::SUPER, "t".into())],
            args: &[ArgType::ActiveWindow],
            menubar: &["Shell"],
        },
        SpawnTab(SpawnTabDomain::DefaultDomain) => CommandDef {
            brief: "New Tab (Default Domain)".into(),
            doc: "Create a new tab in the default domain".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Shell"],
        },
        SpawnTab(SpawnTabDomain::DomainName(name)) => CommandDef {
            brief: format!("New Tab (`{name}` Domain)").into(),
            doc: format!("Create a new tab in the domain named {name}").into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Shell"],
        },
        SpawnTab(SpawnTabDomain::DomainId(id)) => CommandDef {
            brief: format!("New Tab (Domain with id {id})").into(),
            doc: format!("Create a new tab in the domain with id {id}").into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Shell"],
        },
        SpawnCommandInNewTab(cmd) => CommandDef {
            brief: format!("Spawn a new Tab with {cmd:?}").into(),
            doc: format!("Spawn a new Tab with {cmd:?}").into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        SpawnCommandInNewWindow(cmd) => CommandDef {
            brief: format!("Spawn a new Window with {cmd:?}").into(),
            doc: format!("Spawn a new Window with {cmd:?}").into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        ActivateTab(-1) => CommandDef {
            brief: "Activate right-most tab".into(),
            doc: "Activates the tab on the far right".into(),
            keys: vec![(Modifiers::SUPER, "9".into())],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window", "Select Tab"],
        },
        ActivateTab(n) => {
            let n = *n;
            let ordinal = english_ordinal(n);
            let keys = if n >= 0 && n <= 7 {
                vec![(Modifiers::SUPER, (n + 1).to_string())]
            } else {
                vec![]
            };
            CommandDef {
                brief: format!("Activate {ordinal} Tab").into(),
                doc: format!("Activates the {ordinal} tab").into(),
                keys,
                args: &[ArgType::ActiveWindow],
                menubar: &["Window", "Select Tab"],
            }
        }
        ActivatePaneByIndex(n) => {
            let n = *n;
            let ordinal = english_ordinal(n as isize);
            CommandDef {
                brief: format!("Activate {ordinal} Pane").into(),
                doc: format!("Activates the {ordinal} Pane").into(),
                keys: vec![],
                args: &[ArgType::ActiveWindow],
                menubar: &[],
            }
        }
        SetPaneZoomState(true) => CommandDef {
            brief: format!("Zooms the current Pane").into(),
            doc: format!(
                "Places the current pane into the zoomed state, \
                             filling all of the space in the tab"
            )
            .into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        SetPaneZoomState(false) => CommandDef {
            brief: format!("Un-Zooms the current Pane").into(),
            doc: format!("Takes the current pane out of the zoomed state").into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        EmitEvent(name) => CommandDef {
            brief: format!("Emit event `{name}`").into(),
            doc: format!(
                "Emits the named event, causing any \
                             associated event handler(s) to trigger"
            )
            .into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        CloseCurrentTab { confirm: true } => CommandDef {
            brief: "Close current Tab".into(),
            doc: "Closes the current tab, terminating all the \
            processes that are running in its panes."
                .into(),
            keys: vec![(Modifiers::SUPER, "w".into())],
            args: &[ArgType::ActiveTab],
            menubar: &["Shell"],
        },
        CloseCurrentTab { confirm: false } => CommandDef {
            brief: "Close current Tab".into(),
            doc: "Closes the current tab, terminating all the \
            processes that are running in its panes."
                .into(),
            keys: vec![],
            args: &[ArgType::ActiveTab],
            menubar: &[],
        },
        CloseCurrentPane { confirm: true } => CommandDef {
            brief: "Close current Pane".into(),
            doc: "Closes the current pane, terminating the \
            processes that are running inside it."
                .into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell"],
        },
        CloseCurrentPane { confirm: false } => CommandDef {
            brief: "Close current Pane".into(),
            doc: "Closes the current pane, terminating the \
            processes that are running inside it."
                .into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        ActivateTabRelative(-1) => CommandDef {
            brief: "Activate the tab to the left".into(),
            doc: "Activates the tab to the left. If this is the left-most \
            tab then cycles around and activates the right-most tab"
                .into(),
            keys: vec![
                (Modifiers::SUPER.union(Modifiers::SHIFT), "[".into()),
                (Modifiers::CTRL.union(Modifiers::SHIFT), "Tab".into()),
                (Modifiers::CTRL, "PageUp".into()),
            ],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window", "Select Tab"],
        },
        ActivateTabRelative(1) => CommandDef {
            brief: "Activate the tab to the right".into(),
            doc: "Activates the tab to the right. If this is the right-most \
            tab then cycles around and activates the left-most tab"
                .into(),
            keys: vec![
                (Modifiers::SUPER.union(Modifiers::SHIFT), "]".into()),
                (Modifiers::CTRL, "Tab".into()),
                (Modifiers::CTRL, "PageDown".into()),
            ],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window", "Select Tab"],
        },
        ActivateTabRelative(n) => {
            let (direction, amount) = if *n < 0 { ("left", -n) } else { ("right", *n) };
            let ordinal = english_ordinal(amount);
            CommandDef {
                brief: format!("Activate the {ordinal} tab to the {direction}").into(),
                doc: format!(
                    "Activates the {ordinal} tab to the {direction}. \
                         Wraps around to the other end"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActiveWindow],
                menubar: &[],
            }
        }
        ActivateTabRelativeNoWrap(-1) => CommandDef {
            brief: "Activate the tab to the left (no wrapping)".into(),
            doc: "Activates the tab to the left. Stopping at the left-most tab".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        ActivateTabRelativeNoWrap(1) => CommandDef {
            brief: "Activate the tab to the right (no wrapping)".into(),
            doc: "Activates the tab to the right. Stopping at the right-most tab".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &[],
        },
        ActivateTabRelativeNoWrap(n) => {
            let (direction, amount) = if *n < 0 { ("left", -n) } else { ("right", *n) };
            let ordinal = english_ordinal(amount);
            CommandDef {
                brief: format!("Activate the {ordinal} tab to the {direction}").into(),
                doc: format!("Activates the {ordinal} tab to the {direction}").into(),
                keys: vec![],
                args: &[ArgType::ActiveWindow],
                menubar: &[],
            }
        }
        ReloadConfiguration => CommandDef {
            brief: "Reload configuration".into(),
            doc: "Reloads the configuration file".into(),
            keys: vec![(Modifiers::SUPER, "r".into())],
            args: &[],
            menubar: &["WezTerm"],
        },
        QuitApplication => CommandDef {
            brief: "Quit WezTerm".into(),
            doc: "Quits WezTerm".into(),
            keys: vec![(Modifiers::SUPER, "q".into())],
            args: &[],
            menubar: &["WezTerm"],
        },
        MoveTabRelative(-1) => CommandDef {
            brief: "Move tab one place to the left".into(),
            doc: "Rearranges the tabs so that the current tab moves \
            one place to the left"
                .into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "PageUp".into())],
            args: &[ArgType::ActiveTab],
            menubar: &["Window", "Move Tab"],
        },
        MoveTabRelative(1) => CommandDef {
            brief: "Move tab one place to the right".into(),
            doc: "Rearranges the tabs so that the current tab moves \
            one place to the right"
                .into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "PageDown".into())],
            args: &[ArgType::ActiveTab],
            menubar: &["Window", "Move Tab"],
        },
        MoveTabRelative(n) => {
            let (direction, amount) = if *n < 0 {
                ("left", (-n).to_string())
            } else {
                ("right", n.to_string())
            };

            CommandDef {
                brief: format!("Move tab {amount} place(s) to the {direction}").into(),
                doc: format!(
                    "Rearranges the tabs so that the current tab moves \
            {amount} place(s) to the {direction}"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActiveTab],
                menubar: &[],
            }
        }
        MoveTab(n) => {
            let n = (*n) + 1;
            CommandDef {
                brief: format!("Move tab to index {n}").into(),
                doc: format!(
                    "Rearranges the tabs so that the current tab \
                             moves to position {n}"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActiveTab],
                menubar: &[],
            }
        }
        ScrollByPage(amount) => {
            let amount = amount.into_inner();
            if amount == -1.0 {
                CommandDef {
                    brief: "Scroll Up One Page".into(),
                    doc: "Scrolls the viewport up by 1 page".into(),
                    keys: vec![(Modifiers::SHIFT, "PageUp".into())],
                    args: &[ArgType::ActivePane],
                    menubar: &["View"],
                }
            } else if amount == 1.0 {
                CommandDef {
                    brief: "Scroll Down One Page".into(),
                    doc: "Scrolls the viewport down by 1 page".into(),
                    keys: vec![(Modifiers::SHIFT, "PageDown".into())],
                    args: &[ArgType::ActivePane],
                    menubar: &["View"],
                }
            } else if amount < 0.0 {
                let amount = -amount;
                CommandDef {
                    brief: format!("Scroll Up {amount} Page(s)").into(),
                    doc: format!("Scrolls the viewport up by {amount} pages").into(),
                    keys: vec![],
                    args: &[ArgType::ActivePane],
                    menubar: &["View"],
                }
            } else {
                CommandDef {
                    brief: format!("Scroll Down {amount} Page(s)").into(),
                    doc: format!("Scrolls the viewport down by {amount} pages").into(),
                    keys: vec![],
                    args: &[ArgType::ActivePane],
                    menubar: &["View"],
                }
            }
        }
        ScrollByLine(n) => {
            let (direction, amount) = if *n < 0 {
                ("up", (-n).to_string())
            } else {
                ("down", n.to_string())
            };
            CommandDef {
                brief: format!("Scroll {direction} {amount} line(s)").into(),
                doc: format!(
                    "Scrolls the viewport {direction} by \
                             {amount} line(s)"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActivePane],
                menubar: &[],
            }
        }
        ScrollToPrompt(n) => {
            let (direction, amount) = if *n < 0 { ("up", -n) } else { ("down", *n) };
            let ordinal = english_ordinal(amount);
            CommandDef {
                brief: format!("Scroll {direction} {amount} prompt(s)").into(),
                doc: format!(
                    "Scrolls the viewport {direction} to the \
                             {ordinal} semantic prompt zone in that direction"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActivePane],
                menubar: &[],
            }
        }
        ScrollByCurrentEventWheelDelta => CommandDef {
            brief: "Scrolls based on the mouse wheel position \
                in the current mouse event"
                .into(),
            doc: "Scrolls based on the mouse wheel position \
                in the current mouse event"
                .into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        ScrollToBottom => CommandDef {
            brief: "Scroll to the bottom".into(),
            doc: "Scrolls to the bottom of the viewport".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["View"],
        },
        ScrollToTop => CommandDef {
            brief: "Scroll to the top".into(),
            doc: "Scrolls to the top of the viewport".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["View"],
        },
        ActivateCopyMode => CommandDef {
            brief: "Activate Copy Mode".into(),
            doc: "Enter mouse-less copy mode to select text using only \
            the keyboard"
                .into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "x".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        SplitVertical(SpawnCommand {
            domain: SpawnTabDomain::CurrentPaneDomain,
            ..
        }) => CommandDef {
            brief: "Split Vertically (Top/Bottom)".into(),
            doc: "Split the current pane vertically into two panes, by spawning \
            the default program into the bottom half"
                .into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "'".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Shell"],
        },
        SplitHorizontal(SpawnCommand {
            domain: SpawnTabDomain::CurrentPaneDomain,
            ..
        }) => CommandDef {
            brief: "Split Horizontally (Left/Right)".into(),
            doc: "Split the current pane horizontally into two panes, by spawning \
            the default program into the right hand side"
                .into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "5".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Shell"],
        },
        SplitHorizontal(_) => CommandDef {
            brief: "Split Horizontally (Left/Right)".into(),
            doc: "Split the current pane horizontally into two panes, by spawning \
            the default program into the right hand side"
                .into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        SplitVertical(_) => CommandDef {
            brief: "Split Vertically (Top/Bottom)".into(),
            doc: "Split the current pane veritically into two panes, by spawning \
            the default program into the bottom"
                .into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        AdjustPaneSize(PaneDirection::Left, amount) => CommandDef {
            brief: format!("Resize Pane {amount} cells to the Left").into(),
            doc: "Adjusts the closest split divider to the left".into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "LeftArrow".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Resize Pane"],
        },
        AdjustPaneSize(PaneDirection::Right, amount) => CommandDef {
            brief: format!("Resize Pane {amount} cells to the Right").into(),
            doc: "Adjusts the closest split divider to the right".into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "RightArrow".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Resize Pane"],
        },
        AdjustPaneSize(PaneDirection::Up, amount) => CommandDef {
            brief: format!("Resize Pane {amount} cells Upwards").into(),
            doc: "Adjusts the closest split divider towards the top".into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "UpArrow".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Resize Pane"],
        },
        AdjustPaneSize(PaneDirection::Down, amount) => CommandDef {
            brief: format!("Resize Pane {amount} cells Downwards").into(),
            doc: "Adjusts the closest split divider towards the bottom".into(),
            keys: vec![(
                Modifiers::CTRL
                    .union(Modifiers::ALT)
                    .union(Modifiers::SHIFT),
                "DownArrow".into(),
            )],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Resize Pane"],
        },
        AdjustPaneSize(PaneDirection::Next | PaneDirection::Prev, _) => return None,
        ActivatePaneDirection(PaneDirection::Next | PaneDirection::Prev) => return None,
        ActivatePaneDirection(PaneDirection::Left) => CommandDef {
            brief: "Activate Pane Left".into(),
            doc: "Activates the pane to the left of the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "LeftArrow".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Select Pane"],
        },
        ActivatePaneDirection(PaneDirection::Right) => CommandDef {
            brief: "Activate Pane Right".into(),
            doc: "Activates the pane to the right of the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "RightArrow".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Select Pane"],
        },
        ActivatePaneDirection(PaneDirection::Up) => CommandDef {
            brief: "Activate Pane Up".into(),
            doc: "Activates the pane to the top of the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "UpArrow".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Select Pane"],
        },
        ActivatePaneDirection(PaneDirection::Down) => CommandDef {
            brief: "Activate Pane Down".into(),
            doc: "Activates the pane to the bottom of the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "DownArrow".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Select Pane"],
        },
        TogglePaneZoomState => CommandDef {
            brief: "Toggle Pane Zoom".into(),
            doc: "Toggles the zoom state for the current pane".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "z".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Window"],
        },
        ActivateLastTab => CommandDef {
            brief: "Activate the last active tab".into(),
            doc: "If there was no prior active tab, has no effect.".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window", "Select Tab"],
        },
        ClearKeyTableStack => CommandDef {
            brief: "Clear the key table stack".into(),
            doc: "Removes all entries from the stack".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Edit"],
        },
        OpenLinkAtMouseCursor => CommandDef {
            brief: "Open link at mouse cursor".into(),
            doc: "If there is no link under the mouse cursor, has no effect.".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell"],
        },
        ShowLauncherArgs(_) | ShowLauncher => CommandDef {
            brief: "Show the launcher".into(),
            doc: "Shows the launcher menu".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Shell"],
        },
        ShowTabNavigator => CommandDef {
            brief: "Navigate tabs".into(),
            doc: "Shows the tab navigator".into(),
            keys: vec![],
            args: &[ArgType::ActiveWindow],
            menubar: &["Window", "Select Tab"],
        },
        DetachDomain(SpawnTabDomain::CurrentPaneDomain) => CommandDef {
            brief: "Detach the domain of the active pane".into(),
            doc: "Detaches (disconnects from) the domain of the active pane".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell", "Detach"],
        },
        DetachDomain(SpawnTabDomain::DefaultDomain) => CommandDef {
            brief: "Detach the default domain".into(),
            doc: "Detaches (disconnects from) the default domain".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell", "Detach"],
        },
        DetachDomain(SpawnTabDomain::DomainName(name)) => CommandDef {
            brief: format!("Detach the `{name}` domain").into(),
            doc: format!("Detaches (disconnects from) the domain named `{name}`").into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell", "Detach"],
        },
        DetachDomain(SpawnTabDomain::DomainId(id)) => CommandDef {
            brief: format!("Detach the domain with id {id}").into(),
            doc: format!("Detaches (disconnects from) the domain with id {id}").into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell", "Detach"],
        },
        OpenUri(uri) => match uri.as_ref() {
            "https://wezfurlong.org/wezterm/" => CommandDef {
                brief: "Documentation".into(),
                doc: "Visit the wezterm documentation website".into(),
                keys: vec![],
                args: &[],
                menubar: &["Help"],
            },
            "https://github.com/wez/wezterm/discussions/" => CommandDef {
                brief: "Discuss on GitHub".into(),
                doc: "Visit wezterm's GitHub discussion".into(),
                keys: vec![],
                args: &[],
                menubar: &["Help"],
            },
            "https://github.com/wez/wezterm/issues/" => CommandDef {
                brief: "Search or report issue on GitHub".into(),
                doc: "Visit wezterm's GitHub issues".into(),
                keys: vec![],
                args: &[],
                menubar: &["Help"],
            },
            _ => CommandDef {
                brief: format!("Open {uri} in your browser").into(),
                doc: format!("Open {uri} in your browser").into(),
                keys: vec![],
                args: &[],
                menubar: &[],
            },
        },
        SendString(text) => CommandDef {
            brief: format!(
                "Sends `{text}` to the active pane, \
                           as though you typed it"
            )
            .into(),
            doc: format!(
                "Sends `{text}` to the active pane, as \
                         though you typed it"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        SendKey(key) => CommandDef {
            brief: format!(
                "Sends {key:?} to the active pane, \
                           as though you typed it"
            )
            .into(),
            doc: format!(
                "Sends {key:?} to the active pane, \
                         as though you typed it"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        Nop => CommandDef {
            brief: "Does nothing".into(),
            doc: "Has no effect".into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        DisableDefaultAssignment => return None,
        SelectTextAtMouseCursor(mode) => CommandDef {
            brief: format!(
                "Selects text at the mouse cursor \
                           location using {mode:?}"
            )
            .into(),
            doc: format!(
                "Selects text at the mouse cursor \
                         location using {mode:?}"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        ExtendSelectionToMouseCursor(mode) => CommandDef {
            brief: format!(
                "Extends the selection text to the mouse \
                           cursor location using {mode:?}"
            )
            .into(),
            doc: format!(
                "Extends the selection text to the mouse \
                         cursor location using {mode:?}"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        ClearSelection => CommandDef {
            brief: "Clears the selection in the current pane".into(),
            doc: "Clears the selection in the current pane".into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        CompleteSelection(destination) => CommandDef {
            brief: format!("Completes selection, and copy {destination:?}").into(),
            doc: format!(
                "Completes text selection using the mouse, and copies \
                to {destination:?}"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        CompleteSelectionOrOpenLinkAtMouseCursor(destination) => CommandDef {
            brief: format!(
                "Open a URL or Completes selection \
            by copying to {destination:?}"
            )
            .into(),
            doc: format!(
                "If the mouse is over a link, open it, otherwise, completes \
                text selection using the mouse, and copies to {destination:?}"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        StartWindowDrag => CommandDef {
            brief: "Requests a window drag operation from \
                the window environment"
                .into(),
            doc: "Requests a window drag operation from \
                the window environment"
                .into(),
            keys: vec![],
            args: &[],
            menubar: &[],
        },
        Multiple(actions) => {
            let mut brief = String::new();
            for act in actions {
                if !brief.is_empty() {
                    brief.push_str(", ");
                }
                match derive_command_from_key_assignment(act) {
                    Some(cmd) => {
                        brief.push_str(&cmd.brief);
                    }
                    None => {
                        brief.push_str(&format!("{act:?}"));
                    }
                }
            }
            CommandDef {
                brief: brief.into(),
                doc: "Performs multiple nested actions".into(),
                keys: vec![],
                args: &[ArgType::ActivePane],
                menubar: &[],
            }
        }
        SwitchToWorkspace {
            name: None,
            spawn: None,
        } => CommandDef {
            brief: format!(
                "Spawn the default program into a new \
                           workspace and switch to it"
            )
            .into(),
            doc: format!(
                "Spawn the default program into a new \
                         workspace and switch to it"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &["Window", "Workspace"],
        },
        SwitchToWorkspace {
            name: Some(name),
            spawn: None,
        } => CommandDef {
            brief: format!(
                "Switch to workspace `{name}`, spawn the \
                           default program if that workspace doesn't already exist"
            )
            .into(),
            doc: format!(
                "Switch to workspace `{name}`, spawn the \
                         default program if that workspace doesn't already exist"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &["Window", "Workspace"],
        },
        SwitchToWorkspace {
            name: Some(name),
            spawn: Some(prog),
        } => CommandDef {
            brief: format!(
                "Switch to workspace `{name}`, spawn {prog:?} \
                           if that workspace doesn't already exist"
            )
            .into(),
            doc: format!(
                "Switch to workspace `{name}`, spawn {prog:?} \
                         if that workspace doesn't already exist"
            )
            .into(),
            keys: vec![],
            args: &[],
            menubar: &["Window", "Workspace"],
        },
        SwitchToWorkspace {
            name: None,
            spawn: Some(prog),
        } => CommandDef {
            brief: format!("Spawn the {prog:?} into a new workspace and switch to it").into(),
            doc: format!("Spawn the {prog:?} into a new workspace and switch to it").into(),
            keys: vec![],
            args: &[],
            menubar: &["Window", "Workspace"],
        },
        SwitchWorkspaceRelative(n) => {
            let (direction, amount) = if *n < 0 {
                ("previous", -n)
            } else {
                ("next", *n)
            };
            let ordinal = english_ordinal(amount);
            CommandDef {
                brief: format!("Switch to {ordinal} {direction} workspace").into(),
                doc: format!(
                    "Switch to the {ordinal} {direction} workspace, \
                             ordered lexicographically by workspace name"
                )
                .into(),
                keys: vec![],
                args: &[ArgType::ActivePane],
                menubar: &["Window", "Workspace"],
            }
        }
        ActivateKeyTable { name, .. } => CommandDef {
            brief: format!("Activate key table `{name}`").into(),
            doc: format!("Activate key table `{name}`").into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        PopKeyTable => CommandDef {
            brief: "Pop the current key table".into(),
            doc: "Pop the current key table".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        AttachDomain(name) => CommandDef {
            brief: format!("Attach domain `{name}`").into(),
            doc: format!("Attach domain `{name}`").into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &[],
        },
        CopyMode(_) => CommandDef {
            brief: "Activate Copy Mode".into(),
            doc: "Activate Copy Mode".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
        RotatePanes(direction) => CommandDef {
            brief: format!("Rotate panes {direction:?}").into(),
            doc: format!("Rotate panes {direction:?}").into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Window", "Rotate Pane"],
        },
        SplitPane(split) => {
            let direction = split.direction;
            CommandDef {
                brief: format!("Split the current pane {direction:?}").into(),
                doc: format!("Split the current pane {direction:?}").into(),
                keys: vec![],
                args: &[ArgType::ActivePane],
                menubar: &[],
            }
        }
        ResetTerminal => CommandDef {
            brief: "Reset the terminal emulation state in the current pane".into(),
            doc: "Reset the terminal emulation state in the current pane".into(),
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Shell"],
        },
        ActivateCommandPalette => CommandDef {
            brief: "Activate Command Palette".into(),
            doc: "Shows the command palette modal".into(),
            keys: vec![(Modifiers::CTRL.union(Modifiers::SHIFT), "p".into())],
            args: &[ArgType::ActivePane],
            menubar: &["Edit"],
        },
    })
}

/// Returns a list of key assignment actions that should be
/// included in the default key assignments and command palette.
fn compute_default_actions() -> Vec<KeyAssignment> {
    // These are ordered by their position within the various menus
    return vec![
        // ----------------- WezTerm
        ReloadConfiguration,
        #[cfg(target_os = "macos")]
        HideApplication,
        #[cfg(target_os = "macos")]
        QuitApplication,
        // ----------------- Shell
        SpawnTab(SpawnTabDomain::CurrentPaneDomain),
        SpawnWindow,
        SplitVertical(SpawnCommand {
            domain: SpawnTabDomain::CurrentPaneDomain,
            ..Default::default()
        }),
        SplitHorizontal(SpawnCommand {
            domain: SpawnTabDomain::CurrentPaneDomain,
            ..Default::default()
        }),
        CloseCurrentTab { confirm: true },
        CloseCurrentPane { confirm: true },
        DetachDomain(SpawnTabDomain::CurrentPaneDomain),
        ResetTerminal,
        // ----------------- Edit
        #[cfg(not(target_os = "macos"))]
        PasteFrom(ClipboardPasteSource::PrimarySelection),
        #[cfg(not(target_os = "macos"))]
        CopyTo(ClipboardCopyDestination::PrimarySelection),
        CopyTo(ClipboardCopyDestination::Clipboard),
        PasteFrom(ClipboardPasteSource::Clipboard),
        ClearScrollback(ScrollbackEraseMode::ScrollbackOnly),
        ClearScrollback(ScrollbackEraseMode::ScrollbackAndViewport),
        QuickSelect,
        CharSelect(CharSelectArguments::default()),
        ActivateCopyMode,
        ClearKeyTableStack,
        ActivateCommandPalette,
        // ----------------- View
        DecreaseFontSize,
        IncreaseFontSize,
        ResetFontSize,
        ResetFontAndWindowSize,
        ScrollByPage(NotNan::new(-1.0).unwrap()),
        ScrollByPage(NotNan::new(1.0).unwrap()),
        ScrollToTop,
        ScrollToBottom,
        // ----------------- Window
        ToggleFullScreen,
        Hide,
        Search(Pattern::CurrentSelectionOrEmptyString),
        PaneSelect(PaneSelectArguments::default()),
        RotatePanes(RotationDirection::Clockwise),
        RotatePanes(RotationDirection::CounterClockwise),
        ActivateTab(0),
        ActivateTab(1),
        ActivateTab(2),
        ActivateTab(3),
        ActivateTab(4),
        ActivateTab(5),
        ActivateTab(6),
        ActivateTab(7),
        ActivateTab(-1),
        ActivateTabRelative(-1),
        ActivateTabRelative(1),
        MoveTabRelative(-1),
        MoveTabRelative(1),
        AdjustPaneSize(PaneDirection::Left, 1),
        AdjustPaneSize(PaneDirection::Right, 1),
        AdjustPaneSize(PaneDirection::Up, 1),
        AdjustPaneSize(PaneDirection::Down, 1),
        ActivatePaneDirection(PaneDirection::Left),
        ActivatePaneDirection(PaneDirection::Right),
        ActivatePaneDirection(PaneDirection::Up),
        ActivatePaneDirection(PaneDirection::Down),
        TogglePaneZoomState,
        ActivateLastTab,
        ShowLauncher,
        ShowTabNavigator,
        // ----------------- Help
        OpenUri("https://wezfurlong.org/wezterm/".to_string()),
        OpenUri("https://github.com/wez/wezterm/discussions/".to_string()),
        OpenUri("https://github.com/wez/wezterm/issues/".to_string()),
        ShowDebugOverlay,
        // ----------------- Misc
        OpenLinkAtMouseCursor,
    ];
}
