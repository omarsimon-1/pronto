//! The action registry (ADR-017): every user-visible command with its GAction name, title,
//! category, scope, default keys and the places that show it. The primary menu, the command
//! palette, the keyboard shortcuts dialog, the accelerators and the scoped shortcuts are all
//! generated from it, so nothing can show a command that has no action.

/// Where an action lives, and where scoped keys work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// The application: `app.` actions, with or without a window.
    App,
    /// The window: `win.` actions that need no message.
    Window,
    /// The message list: `win.` actions on the selected messages.
    List,
    /// The reader: `win.` actions on the open message.
    Reader,
    /// The composer (M6).
    Composer,
}

impl Scope {
    /// The places a widget can install scoped keys for (`install_scoped`).
    pub const SCOPED: [Self; 3] = [Self::List, Self::Reader, Self::Composer];

    /// The GAction group prefix: `app` or `win`.
    pub const fn group(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Window | Self::List | Self::Reader | Self::Composer => "win",
        }
    }

    /// Whether the scope's actions work anywhere in the window. Their keys with Ctrl or Alt,
    /// and their F-keys, are application accelerators.
    pub const fn is_global(self) -> bool {
        matches!(self, Self::App | Self::Window)
    }

    /// Whether one-key shortcuts may work here. The composer and text fields keep their
    /// letters, so only the list and the reader take them.
    pub const fn allows_one_key(self) -> bool {
        matches!(self, Self::List | Self::Reader)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "the application",
            Self::Window => "the window",
            Self::List => "the message list",
            Self::Reader => "the reader",
            Self::Composer => "the composer",
        }
    }
}

/// The command groups of the palette and the keyboard shortcuts dialog, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Navigation,
    Messages,
    Compose,
    View,
    General,
    /// The account's mail rules (ADR-020, M9).
    Rules,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::Navigation,
        Self::Messages,
        Self::Compose,
        Self::View,
        Self::General,
        Self::Rules,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Messages => "Messages",
            Self::Compose => "Compose",
            Self::View => "View",
            Self::General => "General",
            Self::Rules => "Rules",
        }
    }
}

/// The primary menu's sections, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MenuSection {
    View,
    Settings,
    Updates,
    Quit,
}

impl MenuSection {
    pub const ALL: [Self; 4] = [Self::View, Self::Settings, Self::Updates, Self::Quit];
}

/// The shape of the GAction behind an [`ActionId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Command,
    /// An on/off setting with a boolean state, shown with a check mark.
    Toggle,
}

/// Everything the registry knows about one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    /// The GAction name, stable across releases: `keys.toml` and scripts refer to it.
    pub name: &'static str,
    pub title: &'static str,
    pub category: Category,
    pub scope: Scope,
    pub kind: ActionKind,
    /// Default keys as GTK accelerator strings, a two-key sequence as `"g i"`, in the order the
    /// keyboard shortcuts dialog lists them. `keymap::is_accelerator` decides which of them are
    /// application accelerators; the others are scoped shortcuts that work in `key_scopes`.
    pub keys: &'static [&'static str],
    /// The message list's own keys for the action (GtkListView's arrows): shown, never
    /// installed and never rebound.
    pub list_keys: &'static [&'static str],
    /// Where the scoped keys work.
    pub key_scopes: &'static [Scope],
    pub palette: bool,
    pub menu: Option<MenuSection>,
    /// Enabled only while messages are selected.
    pub needs_selection: bool,
    /// Other words the palette finds the command by, in lower case: "done" finds Archive, and
    /// the palette shows a command found this way as "Archive (done)" (Superhuman pass).
    pub aliases: &'static [&'static str],
    /// Its keys that text fields and the composer use themselves (`keymap::TEXT_KEYS`) yield to
    /// the focused widget instead of being refused: they work anywhere in the window, after the
    /// focused widget, which keeps them where it uses them (Ctrl+K opens the palette, but stays
    /// Insert Link in the composer's editor). See `keymap::Placement::Yielding`.
    pub yields: bool,
    /// Its Tab and Shift+Tab (`keymap::FOCUS_KEYS`) are its own where its keys work: the Split
    /// Inbox's Next and Previous Split in the message list (Superhuman's Tab, the fork's phase
    /// C), which leave the focus to Ctrl+Tab there while the list shows the splits.
    pub takes_tab: bool,
}

/// Scoped keys that work in both the message list and the reader.
const MAIL: &[Scope] = &[Scope::List, Scope::Reader];
const LIST: &[Scope] = &[Scope::List];
const READER: &[Scope] = &[Scope::Reader];
const COMPOSER: &[Scope] = &[Scope::Composer];

const fn command(
    name: &'static str,
    title: &'static str,
    category: Category,
    scope: Scope,
    keys: &'static [&'static str],
) -> ActionSpec {
    ActionSpec {
        name,
        title,
        category,
        scope,
        kind: ActionKind::Command,
        keys,
        list_keys: &[],
        key_scopes: match scope {
            Scope::App | Scope::Window | Scope::List => MAIL,
            Scope::Reader => READER,
            Scope::Composer => COMPOSER,
        },
        palette: true,
        menu: None,
        needs_selection: false,
        aliases: &[],
        yields: false,
        takes_tab: false,
    }
}

/// A command on the selected messages; its keys work in the list and the reader.
const fn on_selection(
    name: &'static str,
    title: &'static str,
    category: Category,
    keys: &'static [&'static str],
) -> ActionSpec {
    ActionSpec {
        needs_selection: true,
        ..command(name, title, category, Scope::List, keys)
    }
}

const fn go_to(
    name: &'static str,
    title: &'static str,
    keys: &'static [&'static str],
) -> ActionSpec {
    command(name, title, Category::Navigation, Scope::Window, keys)
}

/// A filter of the message list: what it shows, whether or not a message is selected; its keys
/// work in the list.
const fn filter(
    name: &'static str,
    title: &'static str,
    keys: &'static [&'static str],
    aliases: &'static [&'static str],
) -> ActionSpec {
    ActionSpec {
        key_scopes: LIST,
        aliases,
        ..command(name, title, Category::View, Scope::List, keys)
    }
}

/// Every user-visible command, in the order the palette and the dialog list them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    NextMessage,
    PreviousMessage,
    NextInConversation,
    PreviousInConversation,
    ScrollMessageDown,
    ScrollMessageUp,
    ScrollMessagePageDown,
    ScrollMessagePageUp,
    OpenMessage,
    BackToList,
    Search,
    GoToInbox,
    GoToStarred,
    GoToSent,
    GoToDrafts,
    GoToArchive,
    GoToFolder,
    GoToAllInboxes,
    GoToAccount1,
    GoToAccount2,
    GoToAccount3,
    GoToAccount4,
    GoToAccount5,
    GoToAccount6,
    GoToAccount7,
    GoToAccount8,
    GoToAccount9,
    Archive,
    Delete,
    Junk,
    MoveTo,
    RemindMe,
    Star,
    ToggleRead,
    MarkRead,
    MarkUnread,
    MarkFolderRead,
    NewFolder,
    RenameFolder,
    DeleteFolder,
    Select,
    SelectAll,
    Undo,
    Print,
    ShowOriginal,
    SaveMessage,
    Compose,
    Reply,
    ReplyAll,
    ReplySender,
    Forward,
    Send,
    EditDraft,
    QuickReply,
    ReplyInComposer,
    AttachFile,
    ShowCcBcc,
    DiscardDraft,
    CloseComposer,
    Bold,
    Italic,
    Underline,
    Strikethrough,
    BulletedList,
    NumberedList,
    InsertLink,
    Quote,
    Indent,
    Outdent,
    ClearFormatting,
    ComposerUndo,
    ComposerRedo,
    PasteAsText,
    ShowUnreadOnly,
    ShowStarredOnly,
    ShowAttachmentsOnly,
    ShowAllMessages,
    ExpandAll,
    CollapseAll,
    ToggleMessage,
    MessageDetails,
    ShowImages,
    AlwaysShowImages,
    Refresh,
    ToggleSidebar,
    ToggleReadingPane,
    GroupByConversation,
    CommandPalette,
    AddAccount,
    EditAccount,
    RemoveAccount,
    KeyboardShortcuts,
    Preferences,
    CustomizeShortcuts,
    RebuildSearchIndex,
    StartAtLogin,
    DefaultMailApp,
    CheckForUpdates,
    Quit,
    EditRules,
    NewRuleFromMessage,
    JunkSender,
    TrustSender,
    ApplyRules,
    // The fork's later additions (the Superhuman pass, phase B), kept together at the end so
    // merges stay trivial: the palette, the dialog and `keys.toml` group them by category.
    SendAndArchive,
    PopOutDraft,
    EditTo,
    EditCc,
    EditBcc,
    ShowFormatting,
    SendLater,
    UseSnippet,
    EditSnippets,
    ShowScheduled,
    // The Split Inbox (the Superhuman pass, phase C).
    SplitInbox,
    NextSplit,
    PreviousSplit,
    GoToImportant,
    GoToOther,
    AlwaysImportant,
    AlwaysOther,
    ResetSenderSplit,
    // Remind Me's UI (the fork's phase C), kept together at the end for the same reason.
    GoToReminders,
    RemoveReminder,
    // Phase C, the contact pane (track K): one block, so the other tracks' blocks merge beside it.
    ContactDetails,
    NextPane,
    PreviousPane,
    // The composer's later commands, one block at the end for the same reason.
    SwitchReply,
    EditFrom,
}

impl ActionId {
    pub const ALL: [Self; 129] = [
        Self::NextMessage,
        Self::PreviousMessage,
        Self::NextInConversation,
        Self::PreviousInConversation,
        Self::ScrollMessageDown,
        Self::ScrollMessageUp,
        Self::ScrollMessagePageDown,
        Self::ScrollMessagePageUp,
        Self::OpenMessage,
        Self::BackToList,
        Self::Search,
        Self::GoToInbox,
        Self::GoToStarred,
        Self::GoToSent,
        Self::GoToDrafts,
        Self::GoToArchive,
        Self::GoToFolder,
        Self::GoToAllInboxes,
        Self::GoToAccount1,
        Self::GoToAccount2,
        Self::GoToAccount3,
        Self::GoToAccount4,
        Self::GoToAccount5,
        Self::GoToAccount6,
        Self::GoToAccount7,
        Self::GoToAccount8,
        Self::GoToAccount9,
        Self::Archive,
        Self::Delete,
        Self::Junk,
        Self::MoveTo,
        Self::RemindMe,
        Self::Star,
        Self::ToggleRead,
        Self::MarkRead,
        Self::MarkUnread,
        Self::MarkFolderRead,
        Self::NewFolder,
        Self::RenameFolder,
        Self::DeleteFolder,
        Self::Select,
        Self::SelectAll,
        Self::Undo,
        Self::Print,
        Self::ShowOriginal,
        Self::SaveMessage,
        Self::Compose,
        Self::Reply,
        Self::ReplyAll,
        Self::ReplySender,
        Self::Forward,
        Self::Send,
        Self::EditDraft,
        Self::QuickReply,
        Self::ReplyInComposer,
        Self::AttachFile,
        Self::ShowCcBcc,
        Self::DiscardDraft,
        Self::CloseComposer,
        Self::Bold,
        Self::Italic,
        Self::Underline,
        Self::Strikethrough,
        Self::BulletedList,
        Self::NumberedList,
        Self::InsertLink,
        Self::Quote,
        Self::Indent,
        Self::Outdent,
        Self::ClearFormatting,
        Self::ComposerUndo,
        Self::ComposerRedo,
        Self::PasteAsText,
        Self::ShowUnreadOnly,
        Self::ShowStarredOnly,
        Self::ShowAttachmentsOnly,
        Self::ShowAllMessages,
        Self::ExpandAll,
        Self::CollapseAll,
        Self::ToggleMessage,
        Self::MessageDetails,
        Self::ShowImages,
        Self::AlwaysShowImages,
        Self::Refresh,
        Self::ToggleSidebar,
        Self::ToggleReadingPane,
        Self::GroupByConversation,
        Self::CommandPalette,
        Self::AddAccount,
        Self::EditAccount,
        Self::RemoveAccount,
        Self::KeyboardShortcuts,
        Self::Preferences,
        Self::CustomizeShortcuts,
        Self::RebuildSearchIndex,
        Self::StartAtLogin,
        Self::DefaultMailApp,
        Self::CheckForUpdates,
        Self::Quit,
        Self::EditRules,
        Self::NewRuleFromMessage,
        Self::JunkSender,
        Self::TrustSender,
        Self::ApplyRules,
        Self::SendAndArchive,
        Self::PopOutDraft,
        Self::EditTo,
        Self::EditCc,
        Self::EditBcc,
        Self::ShowFormatting,
        Self::SendLater,
        Self::UseSnippet,
        Self::EditSnippets,
        Self::ShowScheduled,
        Self::SplitInbox,
        Self::NextSplit,
        Self::PreviousSplit,
        Self::GoToImportant,
        Self::GoToOther,
        Self::AlwaysImportant,
        Self::AlwaysOther,
        Self::ResetSenderSplit,
        Self::GoToReminders,
        Self::RemoveReminder,
        Self::ContactDetails,
        Self::NextPane,
        Self::PreviousPane,
        Self::SwitchReply,
        Self::EditFrom,
    ];

    /// Go to Account `number` (1 to 9), the sidebar's account in that place.
    pub const fn go_to_account(number: usize) -> Option<Self> {
        Some(match number {
            1 => Self::GoToAccount1,
            2 => Self::GoToAccount2,
            3 => Self::GoToAccount3,
            4 => Self::GoToAccount4,
            5 => Self::GoToAccount5,
            6 => Self::GoToAccount6,
            7 => Self::GoToAccount7,
            8 => Self::GoToAccount8,
            9 => Self::GoToAccount9,
            _ => return None,
        })
    }

    pub const fn spec(self) -> ActionSpec {
        use Category::*;
        match self {
            // Down and Up are the message list's own keys; j and k also work in the reader. The
            // list's rows are conversations (the fork's phase B), so j and k are titled for them,
            // as Superhuman's are, and `n` and `p` take the messages' titles (phase C's visual QA;
            // the names in keys.toml stay).
            Self::NextMessage => ActionSpec {
                list_keys: &["Down"],
                ..command(
                    "next-message",
                    "Next Conversation",
                    Navigation,
                    Scope::List,
                    &["j"],
                )
            },
            Self::PreviousMessage => ActionSpec {
                list_keys: &["Up"],
                ..command(
                    "previous-message",
                    "Previous Conversation",
                    Navigation,
                    Scope::List,
                    &["k"],
                )
            },
            // Within the conversation the reader shows (Gmail's `n` and `p`; accepted at the M5
            // gate, 2026-10-06): the next or previous message expands and the column scrolls to it.
            Self::NextInConversation => command(
                "next-in-conversation",
                "Next Message",
                Navigation,
                Scope::List,
                &["n"],
            ),
            Self::PreviousInConversation => command(
                "previous-in-conversation",
                "Previous Message",
                Navigation,
                Scope::List,
                &["p"],
            ),
            // Half a page of the message the reader shows, from the list and the reader (issue #10,
            // accepted 2026-10-19): j and k still move between messages.
            Self::ScrollMessageDown => command(
                "scroll-message-down",
                "Scroll Message Down",
                Navigation,
                Scope::List,
                &["d"],
            ),
            Self::ScrollMessageUp => command(
                "scroll-message-up",
                "Scroll Message Up",
                Navigation,
                Scope::List,
                &["<Shift>d"],
            ),
            // A page of it, in the reader: Space is Select's in the list.
            Self::ScrollMessagePageDown => command(
                "scroll-message-page-down",
                "Scroll Message Page Down",
                Navigation,
                Scope::Reader,
                &["space"],
            ),
            Self::ScrollMessagePageUp => command(
                "scroll-message-page-up",
                "Scroll Message Page Up",
                Navigation,
                Scope::Reader,
                &["<Shift>space"],
            ),
            // `l` (issue #10, accepted 2026-10-19) moves into the message as a file manager does.
            Self::OpenMessage => ActionSpec {
                key_scopes: LIST,
                ..on_selection(
                    "open-message",
                    "Open Message",
                    Navigation,
                    &["Return", "o", "l"],
                )
            },
            // Escape alone since the Superhuman pass: `u` marks read or unread, as there.
            Self::BackToList => command(
                "back-to-list",
                "Back to List",
                Navigation,
                Scope::Reader,
                &["Escape"],
            ),
            // `/` needs Shift on the Norwegian layout, so Ctrl+F is its unshifted alternative.
            Self::Search => ActionSpec {
                aliases: &["find"],
                ..command(
                    "search",
                    "Search",
                    Navigation,
                    Scope::Window,
                    &["slash", "<Control>f"],
                )
            },
            Self::GoToInbox => go_to("go-to-inbox", "Go to Inbox", &["g i"]),
            Self::GoToStarred => ActionSpec {
                aliases: &["flagged"],
                ..go_to("go-to-starred", "Go to Starred", &["g s"])
            },
            Self::GoToSent => go_to("go-to-sent", "Go to Sent", &["g t"]),
            Self::GoToDrafts => go_to("go-to-drafts", "Go to Drafts", &["g d"]),
            // Superhuman's `g e` ("Done") and `g l` (a label, here any folder, in a picker): the
            // shown account's Archive, or the folder Archive moves to where there is none (Gmail's
            // All Mail).
            Self::GoToArchive => ActionSpec {
                aliases: &["done", "all mail"],
                ..go_to("go-to-archive", "Go to Archive", &["g e"])
            },
            Self::GoToFolder => ActionSpec {
                aliases: &["label", "jump to folder"],
                ..go_to("go-to-folder", "Go to Folder…", &["g l"])
            },
            // Issue #12 (accepted 2026-10-19): All Inboxes, and the Inbox of the sidebar's first to
            // ninth account.
            Self::GoToAllInboxes => ActionSpec {
                aliases: &["unified inbox"],
                ..go_to("go-to-all-inboxes", "Go to All Inboxes", &["g a"])
            },
            Self::GoToAccount1 => go_to("go-to-account-1", "Go to Account 1", &["g 1"]),
            Self::GoToAccount2 => go_to("go-to-account-2", "Go to Account 2", &["g 2"]),
            Self::GoToAccount3 => go_to("go-to-account-3", "Go to Account 3", &["g 3"]),
            Self::GoToAccount4 => go_to("go-to-account-4", "Go to Account 4", &["g 4"]),
            Self::GoToAccount5 => go_to("go-to-account-5", "Go to Account 5", &["g 5"]),
            Self::GoToAccount6 => go_to("go-to-account-6", "Go to Account 6", &["g 6"]),
            Self::GoToAccount7 => go_to("go-to-account-7", "Go to Account 7", &["g 7"]),
            Self::GoToAccount8 => go_to("go-to-account-8", "Go to Account 8", &["g 8"]),
            Self::GoToAccount9 => go_to("go-to-account-9", "Go to Account 9", &["g 9"]),
            Self::Archive => ActionSpec {
                aliases: &["done", "mark done"],
                ..on_selection("archive", "Archive", Messages, &["e", "y"])
            },
            Self::Delete => ActionSpec {
                aliases: &["trash", "bin", "remove"],
                ..on_selection("delete", "Delete", Messages, &["numbersign", "Delete"])
            },
            Self::Junk => ActionSpec {
                aliases: &["spam"],
                ..on_selection("junk", "Move to Junk", Messages, &["exclam", "<Control>j"])
            },
            Self::MoveTo => ActionSpec {
                aliases: &["label", "folder", "file"],
                ..on_selection("move-to", "Move to…", Messages, &["v", "<Control><Shift>v"])
            },
            // Superhuman's Remind Me (snooze) on `h` (the fork's phase C): the time picker for
            // the selected conversations, which leave the list for the Reminders folder until
            // then; on a conversation waiting already it changes the time or removes it.
            Self::RemindMe => ActionSpec {
                aliases: &["snooze", "later"],
                ..on_selection("remind-me", "Remind Me…", Messages, &["h"])
            },
            Self::Star => ActionSpec {
                aliases: &["flag"],
                ..on_selection("star", "Star or Unstar", Messages, &["s"])
            },
            // Superhuman's `u`: marks every selected message read unless all are, then unread, as
            // Star does with stars; the palette and the menus say which it would do.
            Self::ToggleRead => ActionSpec {
                aliases: &["seen"],
                ..on_selection("toggle-read", "Mark as Read or Unread", Messages, &["u"])
            },
            Self::MarkRead => on_selection("mark-read", "Mark as Read", Messages, &["<Shift>i"]),
            // Shift+U filters the list since the Superhuman pass; `u` marks unread.
            Self::MarkUnread => on_selection("mark-unread", "Mark as Unread", Messages, &[]),
            // Marks every message of the folder the list shows as read, on the server too (ML-052).
            // No default key until the user picks one; the palette and the sidebar reach it.
            Self::MarkFolderRead => ActionSpec {
                aliases: &["mark all as read"],
                ..command(
                    "mark-folder-read",
                    "Mark Folder as Read",
                    Messages,
                    Scope::Window,
                    &[],
                )
            },
            // Folder management (ADR-020, M9): on the folder right-clicked in the sidebar, else on
            // the folder shown; New Folder from an account's heading goes at the account's top.
            // No default keys until the user picks them (ADR-017).
            Self::NewFolder => ActionSpec {
                aliases: &["create folder", "new label"],
                ..command("new-folder", "New Folder…", Messages, Scope::Window, &[])
            },
            Self::RenameFolder => command(
                "rename-folder",
                "Rename Folder…",
                Messages,
                Scope::Window,
                &[],
            ),
            Self::DeleteFolder => command(
                "delete-folder",
                "Delete Folder…",
                Messages,
                Scope::Window,
                &[],
            ),
            // Toggles the selection of the message under the cursor, for batch actions.
            Self::Select => ActionSpec {
                key_scopes: LIST,
                ..command("select", "Select", Messages, Scope::List, &["x", "space"])
            },
            // Every row of the list, in the select mode (Ctrl+A was swallowed before: rows that
            // haven't loaded have no message yet; triage waits for them, ML-054).
            Self::SelectAll => ActionSpec {
                key_scopes: LIST,
                ..command(
                    "select-all",
                    "Select All",
                    Messages,
                    Scope::List,
                    &["<Control>a"],
                )
            },
            // Undoes the last mail action. Its keys are scoped, so text fields keep Ctrl+Z.
            Self::Undo => command("undo", "Undo", Messages, Scope::List, &["z", "<Control>z"]),
            // The message the reader shows (ML-063). Ctrl+P is the approved mock's; Ctrl+U and
            // Ctrl+S were accepted at the M5 gate (2026-10-06).
            Self::Print => on_selection("print", "Print…", Messages, &["<Control>p"]),
            Self::ShowOriginal => ActionSpec {
                aliases: &["source", "headers"],
                ..on_selection("show-original", "Show Original", Messages, &["<Control>u"])
            },
            Self::SaveMessage => ActionSpec {
                aliases: &["export", "download"],
                ..on_selection("save-message", "Save as .eml…", Messages, &["<Control>s"])
            },
            Self::Compose => ActionSpec {
                aliases: &["new message", "write"],
                ..command(
                    "compose",
                    "Compose",
                    Compose,
                    Scope::Window,
                    &["c", "<Control>n"],
                )
            },
            Self::Reply => on_selection("reply", "Reply", Compose, &["r", "<Control>r"]),
            Self::ReplyAll => on_selection(
                "reply-all",
                "Reply All",
                Compose,
                &["a", "<Control><Shift>r"],
            ),
            Self::ReplySender => on_selection("reply-sender", "Reply to Sender", Compose, &[]),
            Self::Forward => ActionSpec {
                aliases: &["fwd"],
                ..on_selection("forward", "Forward", Compose, &["f", "<Control>l"])
            },
            Self::Send => command(
                "send",
                "Send",
                Compose,
                Scope::Composer,
                &["<Control>Return"],
            ),
            // Opens the selected message of a Drafts folder in the composer (ML-074). No default
            // key until the user picks one.
            Self::EditDraft => on_selection("edit-draft", "Edit Draft", Compose, &[]),
            // The quick reply under the conversation (ML-068): Quick Reply puts the focus in it,
            // answering as Reply does; Superhuman's Enter in an open conversation (the fork's
            // phase B). Open Reply in Composer takes what it holds into the full composer, with
            // the approved mock's Ctrl+Shift+E.
            Self::QuickReply => command(
                "quick-reply",
                "Quick Reply",
                Compose,
                Scope::Reader,
                &["Return"],
            ),
            Self::ReplyInComposer => command(
                "reply-in-composer",
                "Open Reply in Composer",
                Compose,
                Scope::Reader,
                &["<Control><Shift>e"],
            ),
            // The composer window's own commands (ML-067, ML-072). Ctrl+B, Ctrl+I, Ctrl+U,
            // Ctrl+K and Ctrl+Z are the approved mock's; WebKitGTK binds only Ctrl+B, Ctrl+I and
            // Ctrl+A in editable content, so the composer binds the rest (ADR-014); redo's two
            // keys are ADR-014's, and paste as text's Ctrl+Shift+V is a proposal for the gate.
            // Attach File's Ctrl+Shift+A is Thunderbird's and Discard Draft's Ctrl+Shift+D a
            // letter of its own (Superhuman's Ctrl+Shift+, is a shifted symbol).
            Self::AttachFile => command(
                "attach-file",
                "Attach File…",
                Compose,
                Scope::Composer,
                &["<Control><Shift>a"],
            ),
            Self::ShowCcBcc => command(
                "show-cc-bcc",
                "Add Cc and Bcc",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::DiscardDraft => command(
                "discard-draft",
                "Discard Draft",
                Compose,
                Scope::Composer,
                &["<Control><Shift>d"],
            ),
            // Saves the draft and closes the card, as its Close button does: Ctrl+W, the user's
            // pick after a user asked for a key (ADR-017 amendment, 2026-10-19).
            Self::CloseComposer => command(
                "close-composer",
                "Close Composer",
                Compose,
                Scope::Composer,
                &["<Control>w"],
            ),
            Self::Bold => command("bold", "Bold", Compose, Scope::Composer, &["<Control>b"]),
            Self::Italic => command(
                "italic",
                "Italic",
                Compose,
                Scope::Composer,
                &["<Control>i"],
            ),
            Self::Underline => command(
                "underline",
                "Underline",
                Compose,
                Scope::Composer,
                &["<Control>u"],
            ),
            Self::Strikethrough => command(
                "strikethrough",
                "Strikethrough",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::BulletedList => command(
                "bulleted-list",
                "Bulleted List",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::NumberedList => command(
                "numbered-list",
                "Numbered List",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::InsertLink => command(
                "insert-link",
                "Link…",
                Compose,
                Scope::Composer,
                &["<Control>k"],
            ),
            Self::Quote => command("quote", "Quote", Compose, Scope::Composer, &[]),
            Self::Indent => command("indent", "Indent", Compose, Scope::Composer, &[]),
            Self::Outdent => command("outdent", "Outdent", Compose, Scope::Composer, &[]),
            Self::ClearFormatting => command(
                "clear-formatting",
                "Clear Formatting",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::ComposerUndo => command(
                "composer-undo",
                "Undo Typing",
                Compose,
                Scope::Composer,
                &["<Control>z"],
            ),
            Self::ComposerRedo => command(
                "composer-redo",
                "Redo Typing",
                Compose,
                Scope::Composer,
                &["<Control><Shift>z", "<Control>y"],
            ),
            Self::PasteAsText => command(
                "paste-as-text",
                "Paste as Plain Text",
                Compose,
                Scope::Composer,
                &["<Control><Shift>v"],
            ),
            // The list's filters (the chips' until the Superhuman pass): Superhuman's Shift+U and
            // Shift+S in the list; pressed again they show everything. "filter" finds all four.
            Self::ShowUnreadOnly => filter(
                "show-unread-only",
                "Show Unread Only",
                &["<Shift>u"],
                &["filter", "unread filter"],
            ),
            Self::ShowStarredOnly => filter(
                "show-starred-only",
                "Show Starred Only",
                &["<Shift>s"],
                &["filter", "starred filter", "flagged"],
            ),
            Self::ShowAttachmentsOnly => filter(
                "show-attachments-only",
                "Show Attachments Only",
                &[],
                &["filter", "files"],
            ),
            Self::ShowAllMessages => filter(
                "show-all",
                "Show All Messages",
                &[],
                &["filter", "clear filter"],
            ),
            // The conversation the reader shows (ML-057) and its remote content (ML-058). No
            // default keys until the user picks them; the reader's More menu and the palette
            // reach them.
            Self::ExpandAll => command(
                "expand-all",
                "Expand All Messages",
                View,
                Scope::Reader,
                &[],
            ),
            Self::CollapseAll => command(
                "collapse-all",
                "Collapse All Messages",
                View,
                Scope::Reader,
                &[],
            ),
            // On the message of the shown conversation the user is on, as a click on its row or
            // its header, and on its details button; no default key.
            Self::ToggleMessage => command(
                "toggle-message",
                "Expand or Collapse Message",
                View,
                Scope::Reader,
                &[],
            ),
            Self::MessageDetails => ActionSpec {
                aliases: &["headers", "recipients"],
                ..command(
                    "message-details",
                    "Message Details",
                    View,
                    Scope::Reader,
                    &[],
                )
            },
            Self::ShowImages => ActionSpec {
                aliases: &["load images", "remote content"],
                ..command("show-images", "Show Images", View, Scope::Reader, &[])
            },
            // On while the shown message's sender is on the list of senders whose images load.
            Self::AlwaysShowImages => ActionSpec {
                kind: ActionKind::Toggle,
                ..command(
                    "always-show-images",
                    "Always Show Images from This Sender",
                    View,
                    Scope::Reader,
                    &[],
                )
            },
            // Every F-key action has a key without one: the F-row sends media keys without Fn.
            Self::Refresh => ActionSpec {
                menu: Some(MenuSection::View),
                aliases: &["sync", "fetch", "check mail", "get mail"],
                ..command(
                    "refresh",
                    "Refresh",
                    View,
                    Scope::Window,
                    &["<Control><Alt>r", "F5"],
                )
            },
            // F9 is Omarchy's dictation key (voxtype), so it never reaches Pronto (the Superhuman
            // pass, 2026-10-28).
            Self::ToggleSidebar => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::View),
                aliases: &["folders", "hide sidebar", "show sidebar"],
                ..command(
                    "toggle-sidebar",
                    "Toggle Sidebar",
                    View,
                    Scope::Window,
                    &["<Control><Shift>s"],
                )
            },
            // On: Split, the reader beside the list; off: Focused, the conversation in the list's
            // place (the Superhuman pass, 2026-10-28). No default key until the user picks one
            // (ADR-017).
            Self::ToggleReadingPane => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::View),
                ..command(
                    "toggle-reading-pane",
                    "Reading Pane",
                    View,
                    Scope::Window,
                    &[],
                )
            },
            // On: one list row per conversation (the Superhuman pass, phase B); off: one per
            // message, as before. No default key until the user picks one (ADR-017).
            Self::GroupByConversation => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::View),
                aliases: &["threads", "threading", "conversations"],
                ..command(
                    "group-by-conversation",
                    "Group by Conversation",
                    View,
                    Scope::Window,
                    &[],
                )
            },
            // Superhuman's Ctrl+K, from everywhere: it yields to the composer's editor, where it
            // is Insert Link, and the same key closes the palette (Superhuman pass).
            Self::CommandPalette => ActionSpec {
                palette: false,
                menu: Some(MenuSection::View),
                yields: true,
                ..command(
                    "command-palette",
                    "Command Palette",
                    View,
                    Scope::Window,
                    &["<Control>k", "<Control><Shift>p", "F1"],
                )
            },
            // The setup assistant (ML-033), over the window. No default key: the palette, the
            // main menu and the welcome page reach it, and `keys.toml` can bind one.
            Self::AddAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                aliases: &["sign in"],
                ..command("add-account", "Add Account", General, Scope::Window, &[])
            },
            // An account's name and colour in Pronto (the user's UI tweak, 2026-10-08): asks
            // which account, then the engine writes `accounts.toml`. Available while the file
            // lists an account; no default key (ADR-017: keys wait for the user).
            Self::EditAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command("edit-account", "Edit Account", General, Scope::Window, &[])
            },
            // ML-038: asks which account and whether, then the engine deletes it. Available
            // while `accounts.toml` lists an account.
            Self::RemoveAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command(
                    "remove-account",
                    "Remove Account",
                    General,
                    Scope::Window,
                    &[],
                )
            },
            // `app.shortcuts` with Ctrl+? is libadwaita's convention. `?` needs Shift on both
            // layouts, so Ctrl+H is the unshifted alternative (`h` is Remind Me's since the
            // Superhuman pass).
            Self::KeyboardShortcuts => ActionSpec {
                menu: Some(MenuSection::Settings),
                aliases: &["help", "keys", "hotkeys", "cheat sheet"],
                ..command(
                    "shortcuts",
                    "Keyboard Shortcuts",
                    General,
                    Scope::App,
                    &["question", "<Control>question", "<Control>h"],
                )
            },
            Self::Preferences => ActionSpec {
                menu: Some(MenuSection::Settings),
                aliases: &["settings", "options"],
                ..command(
                    "preferences",
                    "Preferences (config.toml)",
                    General,
                    Scope::App,
                    &["<Control>comma"],
                )
            },
            Self::CustomizeShortcuts => ActionSpec {
                menu: Some(MenuSection::Settings),
                aliases: &["keybindings", "rebind keys"],
                ..command(
                    "customize-shortcuts",
                    "Customize Shortcuts (keys.toml)",
                    General,
                    Scope::App,
                    &[],
                )
            },
            // ML-081: indexes every stored message again in the background; search keeps
            // working on the old index until the new one is done.
            Self::RebuildSearchIndex => ActionSpec {
                aliases: &["reindex"],
                ..command(
                    "rebuild-search-index",
                    "Rebuild Search Index",
                    General,
                    Scope::App,
                    &[],
                )
            },
            // ML-084, opt-in autostart (PRD D6): on while Pronto's autostart entry exists. Asks
            // before it writes the entry, the only file Pronto puts outside its own folders.
            Self::StartAtLogin => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::Settings),
                aliases: &["autostart"],
                ..command(
                    "start-at-login",
                    "Start Pronto at Login",
                    General,
                    Scope::App,
                    &[],
                )
            },
            // Asks before it makes Pronto the desktop's handler for mailto: links (PRD C8): the
            // only change Pronto makes outside its own folders, and only on the user's word.
            Self::DefaultMailApp => command(
                "default-mail-app",
                "Make Pronto the Default Mail App…",
                General,
                Scope::App,
                &[],
            ),
            // Asks GitHub for the latest release, only when the user runs it (ADR-021 amendment,
            // 2026-10-20); a menu section of its own, between the settings and Quit. No default
            // key (ADR-017).
            Self::CheckForUpdates => ActionSpec {
                menu: Some(MenuSection::Updates),
                aliases: &["upgrade", "version"],
                ..command(
                    "check-for-updates",
                    "Check for Updates…",
                    General,
                    Scope::App,
                    &[],
                )
            },
            Self::Quit => ActionSpec {
                menu: Some(MenuSection::Quit),
                aliases: &["exit"],
                ..command("quit", "Quit", General, Scope::App, &["<Control>q"])
            },
            // The account's rules (ADR-020, M9): the Rules dialog for the account the list shows,
            // the message commands on the selected messages' sender, and Apply Rules on the
            // folder shown. No default keys until the user picks them (ADR-017).
            Self::EditRules => ActionSpec {
                menu: Some(MenuSection::Settings),
                aliases: &["filters"],
                ..command("edit-rules", "Rules…", Rules, Scope::Window, &[])
            },
            Self::NewRuleFromMessage => on_selection(
                "new-rule-from-message",
                "Create Rule from Message…",
                Rules,
                &[],
            ),
            Self::JunkSender => on_selection(
                "junk-sender",
                "Always Junk Messages from Sender",
                Rules,
                &[],
            ),
            Self::TrustSender => on_selection(
                "trust-sender",
                "Never Junk Messages from Sender",
                Rules,
                &[],
            ),
            Self::ApplyRules => command(
                "apply-rules",
                "Apply Rules to This Folder",
                Rules,
                Scope::Window,
                &[],
            ),
            // Superhuman's reply flow (the fork's phase B): Send and Archive is its Send + Mark
            // Done (Ctrl+Shift+Enter): the reply goes, the conversation is archived as `e` does,
            // and the next one opens; one Undo takes both back.
            Self::SendAndArchive => ActionSpec {
                aliases: &["send and mark done", "send done"],
                ..command(
                    "send-and-archive",
                    "Send and Archive",
                    Compose,
                    Scope::Composer,
                    &["<Control><Shift>Return"],
                )
            },
            // Superhuman's Ctrl+Shift+P: the reply under the conversation goes to a composer card
            // with what it holds, and a reply card comes back under its conversation. The window's
            // Ctrl+Shift+P (the palette) yields to it in a draft.
            Self::PopOutDraft => ActionSpec {
                aliases: &["pop out", "pop in"],
                ..command(
                    "pop-out-draft",
                    "Pop Draft Out or In",
                    Compose,
                    Scope::Composer,
                    &["<Control><Shift>p"],
                )
            },
            // Superhuman's Ctrl+Shift+O, C and B: the draft's recipients, shown and focused.
            Self::EditTo => command(
                "edit-to",
                "Edit To",
                Compose,
                Scope::Composer,
                &["<Control><Shift>o"],
            ),
            Self::EditCc => command(
                "edit-cc",
                "Edit Cc",
                Compose,
                Scope::Composer,
                &["<Control><Shift>c"],
            ),
            Self::EditBcc => command(
                "edit-bcc",
                "Edit Bcc",
                Compose,
                Scope::Composer,
                &["<Control><Shift>b"],
            ),
            // The composer card's formatting toolbar: hidden by default (its keys work all the
            // same), shown from the card's menu; `layout.toml` remembers it. No default key
            // (ADR-017).
            Self::ShowFormatting => ActionSpec {
                kind: ActionKind::Toggle,
                aliases: &["rich text", "styles"],
                ..command(
                    "show-formatting",
                    "Formatting Toolbar",
                    Compose,
                    Scope::Composer,
                    &[],
                )
            },
            // Superhuman's Send Later (Ctrl+Shift+L) and snippets (Ctrl+;), the fork's phase B:
            // the time picker says when the draft goes, and it waits in the outbox until then; a
            // snippet goes into the draft that has the focus, or starts a new message. Ctrl+; is
            // an accelerator, so it works in the composer's editor and the inline reply too: GTK's
            // and WebKit's emoji chooser keep Ctrl+. (`keymap::TEXT_KEYS`). `;` needs Shift on the
            // Norwegian layout, so Ctrl+' is the key without.
            Self::SendLater => ActionSpec {
                aliases: &["schedule send", "send at"],
                ..command(
                    "send-later",
                    "Send Later…",
                    Compose,
                    Scope::Composer,
                    &["<Control><Shift>l"],
                )
            },
            Self::UseSnippet => ActionSpec {
                aliases: &["insert snippet", "template", "canned response"],
                ..command(
                    "use-snippet",
                    "Use Snippet…",
                    Compose,
                    Scope::Window,
                    &["<Control>semicolon", "<Control>apostrophe"],
                )
            },
            // `snippets.toml`, as Preferences opens `config.toml`. No default key (ADR-017).
            Self::EditSnippets => ActionSpec {
                aliases: &["create snippet", "new snippet", "templates"],
                ..command(
                    "edit-snippets",
                    "Edit Snippets (snippets.toml)",
                    Compose,
                    Scope::App,
                    &[],
                )
            },
            // The sends waiting for their time (Send Later), each with Edit and Send Now: the
            // outbox the window shows. No default key (ADR-017).
            Self::ShowScheduled => ActionSpec {
                aliases: &["outbox", "scheduled sends", "send later"],
                ..command(
                    "show-scheduled",
                    "Scheduled Messages",
                    Compose,
                    Scope::Window,
                    &[],
                )
            },
            // The Split Inbox (the fork's phase C, docs/superhuman/split.md "UI"): the Inbox and
            // All Inboxes as two tabs, Important (mail from people) and Other. On by default
            // (`[inbox] split` in config.toml); the toggle is remembered in layout.toml. No
            // default key (ADR-017).
            Self::SplitInbox => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::View),
                aliases: &["tabs", "priority inbox"],
                ..command("split-inbox", "Split Inbox", View, Scope::Window, &[])
            },
            // Superhuman's Tab and Shift+Tab in the list, wrapping; the list's Tab stop moves
            // on with Ctrl+Tab while the splits show, and as before elsewhere (the commands
            // don't apply there, so the key goes on to GTK).
            Self::NextSplit => ActionSpec {
                key_scopes: LIST,
                takes_tab: true,
                aliases: &["next tab"],
                ..command(
                    "next-split",
                    "Next Split",
                    Navigation,
                    Scope::List,
                    &["Tab"],
                )
            },
            Self::PreviousSplit => ActionSpec {
                key_scopes: LIST,
                takes_tab: true,
                aliases: &["previous tab"],
                ..command(
                    "previous-split",
                    "Previous Split",
                    Navigation,
                    Scope::List,
                    &["<Shift>Tab"],
                )
            },
            // Superhuman's `g i` is Go to Inbox's, which shows the Important split; `g o` the
            // Other one.
            Self::GoToImportant => ActionSpec {
                aliases: &["primary"],
                ..go_to("go-to-important", "Go to Important", &[])
            },
            Self::GoToOther => ActionSpec {
                aliases: &["newsletters", "notifications"],
                ..go_to("go-to-other", "Go to Other", &["g o"])
            },
            // The user's side for the selected conversation's sender (its exact address), which
            // wins over everything else; the reader's More menu has them too. No default keys.
            Self::AlwaysImportant => ActionSpec {
                aliases: &["vip", "move sender to important"],
                ..on_selection("always-important", "Always Important", Messages, &[])
            },
            Self::AlwaysOther => ActionSpec {
                aliases: &["move sender to other", "not important"],
                ..on_selection("always-other", "Always Other", Messages, &[])
            },
            Self::ResetSenderSplit => ActionSpec {
                aliases: &["forget sender's side"],
                ..on_selection("reset-sender-split", "Reset Sender Split", Messages, &[])
            },
            // Remind Me's view (phase C): the conversations waiting in the shown account's
            // Reminders folder, the soonest due first; Superhuman's `g h`.
            Self::GoToReminders => ActionSpec {
                aliases: &["snoozed", "reminded"],
                ..go_to("go-to-reminders", "Go to Reminders", &["g h"])
            },
            // Ends the selected conversations' reminders: one waiting comes back to its Inbox
            // now, one back already stops sorting by its return. Superhuman's Shift+E (Mark
            // Not Done) where that means bringing a reminder back; Undo restores it.
            Self::RemoveReminder => ActionSpec {
                aliases: &["mark not done", "not done", "unsnooze", "cancel reminder"],
                ..on_selection(
                    "remove-reminder",
                    "Remove Reminder",
                    Messages,
                    &["<Shift>e"],
                )
            },
            // Superhuman's right-hand pane, local only (phase C): the person behind the open
            // conversation in the reading column's right margin, where it has room. On by
            // default; `layout.toml` remembers it. `i` (info) is free in the list and the reader
            // on both layouts: Superhuman's own `i` expands an AI summary Pronto doesn't have,
            // and its pane key `0` switches to a calendar, which Pronto doesn't have either.
            Self::ContactDetails => ActionSpec {
                kind: ActionKind::Toggle,
                aliases: &["person", "sender", "profile", "right pane"],
                ..command(
                    "contact-details",
                    "Contact Details",
                    View,
                    Scope::Window,
                    &["i"],
                )
            },
            // The keyboard focus from pane to pane, the panes on screen in reading order, round
            // (phase C): the sidebar, the list, the conversation, the contact pane. F6 and
            // Shift+F6 as in GTK's panes and most mail clients; Ctrl+Alt with an arrow for
            // keyboards whose F-keys need Fn (only F-keys fail the key audit). The way into
            // the contact pane, and out of the list where Tab switches the split Inbox's tabs.
            Self::NextPane => ActionSpec {
                aliases: &["focus next pane", "cycle panes"],
                ..command(
                    "next-pane",
                    "Next Pane",
                    Navigation,
                    Scope::Window,
                    &["<Control><Alt>Right", "F6"],
                )
            },
            Self::PreviousPane => ActionSpec {
                aliases: &["focus previous pane"],
                ..command(
                    "previous-pane",
                    "Previous Pane",
                    Navigation,
                    Scope::Window,
                    &["<Control><Alt>Left", "<Shift>F6"],
                )
            },
            // An open reply answers the sender alone or everyone: the other of the two, with
            // the recipients the compose rules plan for it. Ctrl+Shift+R is Reply All's own.
            Self::SwitchReply => ActionSpec {
                aliases: &["toggle reply all", "reply to everyone", "reply to sender"],
                ..command(
                    "switch-reply",
                    "Switch Reply or Reply All",
                    Compose,
                    Scope::Composer,
                    &["<Control><Alt>a"],
                )
            },
            // Superhuman's Ctrl+Shift+F: the From line's senders, when there is more than one.
            Self::EditFrom => ActionSpec {
                aliases: &["change sender", "send from", "account"],
                ..command(
                    "edit-from",
                    "Edit From",
                    Compose,
                    Scope::Composer,
                    &["<Control><Shift>f"],
                )
            },
        }
    }

    pub const fn name(self) -> &'static str {
        self.spec().name
    }

    pub const fn title(self) -> &'static str {
        self.spec().title
    }

    pub const fn category(self) -> Category {
        self.spec().category
    }

    pub const fn scope(self) -> Scope {
        self.spec().scope
    }

    pub const fn kind(self) -> ActionKind {
        self.spec().kind
    }

    /// The name GTK activates: `app.quit`, `win.archive`.
    pub fn detailed_name(self) -> String {
        format!("{}.{}", self.scope().group(), self.name())
    }

    /// The action named `name` (`archive`), as `keys.toml` writes it.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.name() == name)
    }

    /// The action behind a detailed name (`win.archive`).
    pub fn from_detailed_name(detailed: &str) -> Option<Self> {
        let (group, name) = detailed.split_once('.')?;
        Self::from_name(name).filter(|id| id.scope().group() == group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_lists_every_action_once_in_declaration_order() {
        let names: HashSet<_> = ActionId::ALL.iter().map(|id| id.name()).collect();
        assert_eq!(names.len(), ActionId::ALL.len());
        let mut sorted = ActionId::ALL;
        sorted.sort();
        assert_eq!(sorted, ActionId::ALL);
        for id in ActionId::ALL {
            assert_eq!(ActionId::from_name(id.name()), Some(id));
            assert_eq!(ActionId::from_detailed_name(&id.detailed_name()), Some(id));
        }
        assert_eq!(ActionId::from_name("no-such-action"), None);
        assert_eq!(ActionId::from_detailed_name("app.archive"), None);
        assert_eq!(ActionId::from_detailed_name("archive"), None);
    }

    #[test]
    fn names_are_kebab_case_gaction_names() {
        for id in ActionId::ALL {
            let name = id.name();
            assert!(!name.starts_with('-') && !name.ends_with('-'), "{name}");
            assert!(!name.contains("--"), "{name}");
            assert!(name.starts_with(|c: char| c.is_ascii_lowercase()), "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name}"
            );
        }
    }

    #[test]
    fn titles_are_present_and_unique() {
        let titles: HashSet<_> = ActionId::ALL.iter().map(|id| id.title()).collect();
        assert_eq!(titles.len(), ActionId::ALL.len());
        for id in ActionId::ALL {
            assert!(!id.title().trim().is_empty(), "{id:?}");
        }
    }

    #[test]
    fn categories_follow_declaration_order() {
        let mut last = Category::ALL[0];
        // The fork's later additions sit after Apply Rules whatever their category (merges stay
        // trivial); the palette, the dialog and keys.toml group by category all the same.
        for id in ActionId::ALL
            .into_iter()
            .take_while(|id| *id <= ActionId::ApplyRules)
        {
            assert!(
                id.category() >= last,
                "{id:?} is out of its category's place"
            );
            last = id.category();
        }
        for category in Category::ALL {
            assert!(
                ActionId::ALL.iter().any(|id| id.category() == category),
                "{category:?} is empty"
            );
        }
    }

    #[test]
    fn scopes_and_groups() {
        for id in ActionId::ALL {
            let spec = id.spec();
            let group = if spec.scope == Scope::App {
                "app"
            } else {
                "win"
            };
            assert_eq!(spec.scope.group(), group, "{id:?}");
            assert!(!spec.key_scopes.is_empty(), "{id:?}");
            for scope in spec.key_scopes {
                assert!(
                    Scope::SCOPED.contains(scope),
                    "{id:?} has keys in {scope:?}"
                );
            }
            match spec.scope {
                Scope::App | Scope::Window => assert_eq!(spec.key_scopes, MAIL, "{id:?}"),
                Scope::List => assert!(spec.key_scopes == MAIL || spec.key_scopes == LIST),
                Scope::Reader => assert_eq!(spec.key_scopes, READER, "{id:?}"),
                Scope::Composer => assert_eq!(spec.key_scopes, COMPOSER, "{id:?}"),
            }
            if !spec.list_keys.is_empty() {
                assert!(spec.key_scopes.contains(&Scope::List), "{id:?}");
            }
        }
        assert_eq!(ActionId::Quit.detailed_name(), "app.quit");
        assert_eq!(ActionId::KeyboardShortcuts.detailed_name(), "app.shortcuts");
        assert_eq!(ActionId::Archive.detailed_name(), "win.archive");
        assert_eq!(ActionId::Send.detailed_name(), "win.send");
        assert!(Scope::List.allows_one_key() && Scope::Reader.allows_one_key());
        assert!(!Scope::Composer.allows_one_key() && !Scope::Window.allows_one_key());
    }

    #[test]
    fn every_action_is_reachable() {
        for id in ActionId::ALL {
            let spec = id.spec();
            assert!(spec.palette || spec.menu.is_some(), "{id:?} is unreachable");
        }
        let menu: Vec<ActionId> = ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().menu.is_some())
            .collect();
        assert_eq!(
            menu,
            [
                ActionId::Refresh,
                ActionId::ToggleSidebar,
                ActionId::ToggleReadingPane,
                ActionId::GroupByConversation,
                ActionId::CommandPalette,
                ActionId::AddAccount,
                ActionId::EditAccount,
                ActionId::RemoveAccount,
                ActionId::KeyboardShortcuts,
                ActionId::Preferences,
                ActionId::CustomizeShortcuts,
                ActionId::StartAtLogin,
                ActionId::CheckForUpdates,
                ActionId::Quit,
                ActionId::EditRules,
                ActionId::SplitInbox,
            ]
        );
        assert!(!ActionId::CommandPalette.spec().palette);
        assert!(ActionId::AddAccount.spec().palette);
        assert_eq!(ActionId::AddAccount.detailed_name(), "win.add-account");
        assert_eq!(ActionId::ToggleSidebar.kind(), ActionKind::Toggle);
        assert_eq!(ActionId::ToggleReadingPane.kind(), ActionKind::Toggle);
        assert_eq!(
            ActionId::ToggleReadingPane.detailed_name(),
            "win.toggle-reading-pane"
        );
        assert_eq!(ActionId::StartAtLogin.kind(), ActionKind::Toggle);
        assert_eq!(ActionId::StartAtLogin.detailed_name(), "app.start-at-login");
    }

    #[test]
    fn only_message_commands_need_a_selection() {
        let needs: Vec<ActionId> = ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().needs_selection)
            .collect();
        assert_eq!(
            needs,
            [
                ActionId::OpenMessage,
                ActionId::Archive,
                ActionId::Delete,
                ActionId::Junk,
                ActionId::MoveTo,
                ActionId::RemindMe,
                ActionId::Star,
                ActionId::ToggleRead,
                ActionId::MarkRead,
                ActionId::MarkUnread,
                ActionId::Print,
                ActionId::ShowOriginal,
                ActionId::SaveMessage,
                ActionId::Reply,
                ActionId::ReplyAll,
                ActionId::ReplySender,
                ActionId::Forward,
                ActionId::EditDraft,
                ActionId::NewRuleFromMessage,
                ActionId::JunkSender,
                ActionId::TrustSender,
                ActionId::AlwaysImportant,
                ActionId::AlwaysOther,
                ActionId::ResetSenderSplit,
                ActionId::RemoveReminder,
            ]
        );
        for id in needs {
            assert_eq!(id.scope(), Scope::List, "{id:?}");
        }
    }

    /// DESIGN.md's keymap table (2026-10-04), plus the keys this registry adds to it.
    #[test]
    fn the_design_keymap() {
        let keys = |id: ActionId| id.spec().keys.to_vec();
        assert_eq!(keys(ActionId::NextMessage), ["j"]);
        assert_eq!(ActionId::NextMessage.spec().list_keys, ["Down"]);
        assert_eq!(keys(ActionId::PreviousMessage), ["k"]);
        assert_eq!(ActionId::PreviousMessage.spec().list_keys, ["Up"]);
        assert_eq!(keys(ActionId::OpenMessage), ["Return", "o", "l"]);
        // The Superhuman pass: `u` marks read or unread, so Escape alone goes back.
        assert_eq!(keys(ActionId::BackToList), ["Escape"]);
        assert_eq!(keys(ActionId::Compose), ["c", "<Control>n"]);
        assert_eq!(keys(ActionId::Reply), ["r", "<Control>r"]);
        assert_eq!(keys(ActionId::ReplyAll), ["a", "<Control><Shift>r"]);
        assert_eq!(keys(ActionId::Forward), ["f", "<Control>l"]);
        assert_eq!(keys(ActionId::Archive), ["e", "y"]);
        assert_eq!(keys(ActionId::Delete), ["numbersign", "Delete"]);
        assert_eq!(keys(ActionId::Junk), ["exclam", "<Control>j"]);
        assert_eq!(keys(ActionId::MoveTo), ["v", "<Control><Shift>v"]);
        assert_eq!(keys(ActionId::Star), ["s"]);
        assert_eq!(keys(ActionId::MarkRead), ["<Shift>i"]);
        // The Superhuman pass: `u` toggles; Shift+U shows the unread mail, Shift+S the starred.
        assert_eq!(keys(ActionId::ToggleRead), ["u"]);
        assert!(keys(ActionId::MarkUnread).is_empty());
        assert_eq!(keys(ActionId::ShowUnreadOnly), ["<Shift>u"]);
        assert_eq!(keys(ActionId::ShowStarredOnly), ["<Shift>s"]);
        assert!(keys(ActionId::ShowAttachmentsOnly).is_empty());
        assert!(keys(ActionId::ShowAllMessages).is_empty());
        // `h` is Remind Me (phase C), `g h` its view and Shift+E Superhuman's Mark Not Done,
        // which removes a reminder; `g e` is Superhuman's Done, `g l` its label picker.
        assert_eq!(keys(ActionId::RemindMe), ["h"]);
        assert_eq!(keys(ActionId::GoToReminders), ["g h"]);
        assert_eq!(keys(ActionId::RemoveReminder), ["<Shift>e"]);
        assert!(ActionId::RemoveReminder.spec().needs_selection);
        assert_eq!(ActionId::GoToReminders.category(), Category::Navigation);
        assert_eq!(keys(ActionId::GoToArchive), ["g e"]);
        assert_eq!(keys(ActionId::GoToFolder), ["g l"]);
        assert_eq!(keys(ActionId::SelectAll), ["<Control>a"]);
        assert_eq!(ActionId::SelectAll.spec().key_scopes, LIST);
        assert_eq!(keys(ActionId::Select), ["x", "space"]);
        assert_eq!(keys(ActionId::Search), ["slash", "<Control>f"]);
        assert_eq!(keys(ActionId::GoToInbox), ["g i"]);
        assert_eq!(keys(ActionId::GoToStarred), ["g s"]);
        assert_eq!(keys(ActionId::GoToSent), ["g t"]);
        assert_eq!(keys(ActionId::GoToDrafts), ["g d"]);
        // Issues #10 and #12, accepted 2026-10-19.
        assert_eq!(keys(ActionId::ScrollMessageDown), ["d"]);
        assert_eq!(keys(ActionId::ScrollMessageUp), ["<Shift>d"]);
        // A page at a time in the reader, where Space isn't Select's.
        assert_eq!(keys(ActionId::ScrollMessagePageDown), ["space"]);
        assert_eq!(keys(ActionId::ScrollMessagePageUp), ["<Shift>space"]);
        assert_eq!(
            ActionId::ScrollMessagePageDown.spec().key_scopes,
            [Scope::Reader]
        );
        assert_eq!(keys(ActionId::GoToAllInboxes), ["g a"]);
        for number in 1..=9 {
            let id = ActionId::go_to_account(number).unwrap();
            assert_eq!(keys(id), [format!("g {number}")], "{id:?}");
            assert_eq!(id.name(), format!("go-to-account-{number}"));
        }
        assert_eq!(ActionId::go_to_account(0), None);
        assert_eq!(ActionId::go_to_account(10), None);
        assert_eq!(keys(ActionId::Send), ["<Control>Return"]);
        assert_eq!(keys(ActionId::Undo), ["z", "<Control>z"]);
        // Superhuman's Ctrl+K first; it yields to the composer's Insert Link.
        assert_eq!(
            keys(ActionId::CommandPalette),
            ["<Control>k", "<Control><Shift>p", "F1"]
        );
        assert!(ActionId::CommandPalette.spec().yields);
        assert!(
            ActionId::ALL
                .iter()
                .all(|id| *id == ActionId::CommandPalette || !id.spec().yields),
            "only the palette yields"
        );
        // DESIGN lists `?` and Ctrl+?; both need Shift, so Ctrl+H is added (`h` was, until the
        // Superhuman pass gave it to Remind Me).
        assert_eq!(
            keys(ActionId::KeyboardShortcuts),
            ["question", "<Control>question", "<Control>h"]
        );
        // Not in DESIGN's table (proposed).
        assert_eq!(keys(ActionId::Preferences), ["<Control>comma"]);
        assert_eq!(keys(ActionId::Quit), ["<Control>q"]);
        assert_eq!(keys(ActionId::Refresh), ["<Control><Alt>r", "F5"]);
        // F9 is Omarchy's dictation key (`keymap::GRABBED`).
        assert_eq!(keys(ActionId::ToggleSidebar), ["<Control><Shift>s"]);
        assert!(keys(ActionId::ToggleReadingPane).is_empty());
        assert!(keys(ActionId::GroupByConversation).is_empty());
        assert!(keys(ActionId::CustomizeShortcuts).is_empty());
        // No default key until the user picks one (ADR-017: keys wait for the user).
        assert!(keys(ActionId::AddAccount).is_empty());
        assert!(keys(ActionId::EditAccount).is_empty());
        assert!(keys(ActionId::MarkFolderRead).is_empty());
        for id in [
            ActionId::NewFolder,
            ActionId::RenameFolder,
            ActionId::DeleteFolder,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
            assert_eq!(id.scope(), Scope::Window, "{id:?}");
            assert!(id.spec().palette && id.spec().menu.is_none(), "{id:?}");
        }
        assert_eq!(keys(ActionId::Print), ["<Control>p"]);
        assert_eq!(keys(ActionId::ShowOriginal), ["<Control>u"]);
        assert_eq!(keys(ActionId::SaveMessage), ["<Control>s"]);
        assert_eq!(keys(ActionId::NextInConversation), ["n"]);
        assert_eq!(keys(ActionId::PreviousInConversation), ["p"]);
        for id in [
            ActionId::ExpandAll,
            ActionId::CollapseAll,
            ActionId::ShowImages,
            ActionId::AlwaysShowImages,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
        }
        assert_eq!(ActionId::AlwaysShowImages.kind(), ActionKind::Toggle);
        // M6: the mock's composer and quick reply keys, ADR-014's redo keys, and paste as
        // text's proposed Ctrl+Shift+V.
        assert_eq!(keys(ActionId::Bold), ["<Control>b"]);
        assert_eq!(keys(ActionId::Italic), ["<Control>i"]);
        assert_eq!(keys(ActionId::Underline), ["<Control>u"]);
        assert_eq!(keys(ActionId::InsertLink), ["<Control>k"]);
        assert_eq!(keys(ActionId::ComposerUndo), ["<Control>z"]);
        assert_eq!(
            keys(ActionId::ComposerRedo),
            ["<Control><Shift>z", "<Control>y"]
        );
        assert_eq!(keys(ActionId::PasteAsText), ["<Control><Shift>v"]);
        assert_eq!(keys(ActionId::ReplyInComposer), ["<Control><Shift>e"]);
        assert_eq!(keys(ActionId::CloseComposer), ["<Control>w"]);
        // The fork's phase B: Superhuman's reply flow. Enter in an open conversation replies
        // under it; Ctrl+Shift+Enter is Send + Done; Ctrl+Shift+P pops a draft out or in;
        // Ctrl+Shift+O, C and B edit the recipients.
        assert_eq!(keys(ActionId::QuickReply), ["Return"]);
        assert_eq!(ActionId::QuickReply.spec().key_scopes, READER);
        assert_eq!(keys(ActionId::SendAndArchive), ["<Control><Shift>Return"]);
        assert_eq!(keys(ActionId::PopOutDraft), ["<Control><Shift>p"]);
        assert_eq!(keys(ActionId::EditTo), ["<Control><Shift>o"]);
        assert_eq!(keys(ActionId::EditCc), ["<Control><Shift>c"]);
        assert_eq!(keys(ActionId::EditBcc), ["<Control><Shift>b"]);
        assert!(keys(ActionId::ShowFormatting).is_empty());
        assert_eq!(ActionId::ShowFormatting.kind(), ActionKind::Toggle);
        // Send Later and snippets: Superhuman's Ctrl+Shift+L and Ctrl+;, with Ctrl+' where `;`
        // needs Shift (the Norwegian layout).
        assert_eq!(keys(ActionId::SendLater), ["<Control><Shift>l"]);
        assert_eq!(ActionId::SendLater.scope(), Scope::Composer);
        assert_eq!(
            keys(ActionId::UseSnippet),
            ["<Control>semicolon", "<Control>apostrophe"]
        );
        assert_eq!(ActionId::UseSnippet.scope(), Scope::Window);
        assert!(keys(ActionId::EditSnippets).is_empty());
        assert_eq!(ActionId::EditSnippets.scope(), Scope::App);
        assert!(keys(ActionId::ShowScheduled).is_empty());
        // Phase C: the contact pane's toggle, `i` in the list and the reader.
        assert_eq!(keys(ActionId::ContactDetails), ["i"]);
        assert_eq!(ActionId::ContactDetails.kind(), ActionKind::Toggle);
        assert_eq!(ActionId::ContactDetails.scope(), Scope::Window);
        assert_eq!(ActionId::ContactDetails.category(), Category::View);
        assert!(ActionId::ContactDetails.spec().palette);
        assert_eq!(ActionId::ContactDetails.spec().menu, None);
        assert_eq!(
            ActionId::ContactDetails.detailed_name(),
            "win.contact-details"
        );
        // Phase C: pane cycling, F6 and Shift+F6 with a key that needs no Fn, anywhere in the
        // window (application accelerators: a draft's editor and the search field too).
        assert_eq!(keys(ActionId::NextPane), ["<Control><Alt>Right", "F6"]);
        assert_eq!(
            keys(ActionId::PreviousPane),
            ["<Control><Alt>Left", "<Shift>F6"]
        );
        for id in [ActionId::NextPane, ActionId::PreviousPane] {
            assert_eq!(id.scope(), Scope::Window);
            assert_eq!(id.category(), Category::Navigation);
            assert_eq!(id.kind(), ActionKind::Command);
            assert!(id.spec().palette);
            assert_eq!(id.spec().menu, None);
            for key in crate::keymap::Keymap::default().keys(id) {
                assert!(crate::keymap::is_accelerator(id, &key), "{id:?} {key:?}");
            }
        }
        assert_eq!(ActionId::NextPane.detailed_name(), "win.next-pane");
        assert_eq!(ActionId::PreviousPane.detailed_name(), "win.previous-pane");
        for id in [
            ActionId::SendLater,
            ActionId::UseSnippet,
            ActionId::EditSnippets,
            ActionId::ShowScheduled,
        ] {
            assert_eq!(id.category(), Category::Compose, "{id:?}");
            assert!(id.spec().palette, "{id:?}");
        }
        for id in [
            ActionId::SendAndArchive,
            ActionId::PopOutDraft,
            ActionId::EditTo,
            ActionId::EditCc,
            ActionId::EditBcc,
            ActionId::ShowFormatting,
        ] {
            assert_eq!(id.scope(), Scope::Composer, "{id:?}");
            assert_eq!(id.category(), Category::Compose, "{id:?}");
        }
        // The composer's later keys: Attach File, Discard Draft, Edit From and the switch
        // between Reply and Reply All.
        assert_eq!(keys(ActionId::AttachFile), ["<Control><Shift>a"]);
        assert_eq!(keys(ActionId::DiscardDraft), ["<Control><Shift>d"]);
        assert_eq!(keys(ActionId::EditFrom), ["<Control><Shift>f"]);
        assert_eq!(keys(ActionId::SwitchReply), ["<Control><Alt>a"]);
        for id in [ActionId::EditFrom, ActionId::SwitchReply] {
            assert_eq!(id.scope(), Scope::Composer, "{id:?}");
            assert_eq!(id.category(), Category::Compose, "{id:?}");
            assert!(id.spec().palette, "{id:?}");
        }
        for id in [
            ActionId::EditDraft,
            ActionId::ShowCcBcc,
            ActionId::Strikethrough,
            ActionId::BulletedList,
            ActionId::NumberedList,
            ActionId::Quote,
            ActionId::Indent,
            ActionId::Outdent,
            ActionId::ClearFormatting,
            ActionId::DefaultMailApp,
            ActionId::RebuildSearchIndex,
            ActionId::StartAtLogin,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
        }
        // M9's rules (ADR-020): no default keys until the user picks them.
        for id in [
            ActionId::EditRules,
            ActionId::NewRuleFromMessage,
            ActionId::JunkSender,
            ActionId::TrustSender,
            ActionId::ApplyRules,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
            assert_eq!(id.category(), Category::Rules, "{id:?}");
            assert!(id.spec().palette, "{id:?}");
        }
        assert_eq!(ActionId::EditRules.detailed_name(), "win.edit-rules");
        assert_eq!(ActionId::ApplyRules.detailed_name(), "win.apply-rules");
        // The Split Inbox (phase C): Superhuman's Tab and Shift+Tab in the list, `g o` for
        // Other (`g i`, Go to Inbox, shows Important); the rest from the palette.
        assert_eq!(keys(ActionId::NextSplit), ["Tab"]);
        assert_eq!(keys(ActionId::PreviousSplit), ["<Shift>Tab"]);
        for id in [ActionId::NextSplit, ActionId::PreviousSplit] {
            assert_eq!(id.spec().key_scopes, LIST, "{id:?}");
            assert!(id.spec().takes_tab, "{id:?}");
            assert!(!id.spec().needs_selection, "{id:?}");
        }
        assert!(
            ActionId::ALL.iter().all(|id| {
                matches!(id, ActionId::NextSplit | ActionId::PreviousSplit) || !id.spec().takes_tab
            }),
            "only the splits take Tab"
        );
        assert_eq!(keys(ActionId::GoToOther), ["g o"]);
        assert!(keys(ActionId::GoToImportant).is_empty());
        assert_eq!(ActionId::SplitInbox.kind(), ActionKind::Toggle);
        for id in [
            ActionId::SplitInbox,
            ActionId::AlwaysImportant,
            ActionId::AlwaysOther,
            ActionId::ResetSenderSplit,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
            assert!(id.spec().palette, "{id:?}");
        }
    }

    #[test]
    fn aliases_are_lower_case_words_of_their_own() {
        for id in ActionId::ALL {
            let spec = id.spec();
            let title = spec.title.to_lowercase();
            let mut seen = HashSet::new();
            for alias in spec.aliases {
                assert!(
                    !alias.is_empty() && alias.trim() == *alias,
                    "{id:?}: {alias:?}"
                );
                assert_eq!(alias.to_lowercase(), *alias, "{id:?}: {alias}");
                assert!(seen.insert(*alias), "{id:?}: {alias} twice");
                assert!(!title.contains(alias), "{id:?}: {alias} is in the title");
            }
        }
        // The words the Superhuman pass promises.
        let finds = |alias: &str, id: ActionId| id.spec().aliases.contains(&alias);
        assert!(finds("done", ActionId::Archive));
        assert!(finds("trash", ActionId::Delete) && finds("bin", ActionId::Delete));
        assert!(finds("spam", ActionId::Junk));
        assert!(finds("label", ActionId::MoveTo) && finds("folder", ActionId::MoveTo));
        assert!(finds("flag", ActionId::Star));
        assert!(finds("snooze", ActionId::RemindMe) && finds("later", ActionId::RemindMe));
        assert!(finds("settings", ActionId::Preferences));
        assert!(finds("sync", ActionId::Refresh) && finds("fetch", ActionId::Refresh));
        for id in [
            ActionId::ShowUnreadOnly,
            ActionId::ShowStarredOnly,
            ActionId::ShowAttachmentsOnly,
            ActionId::ShowAllMessages,
        ] {
            assert!(finds("filter", id), "{id:?}");
            assert_eq!(id.spec().key_scopes, LIST, "{id:?}");
            assert!(!id.spec().needs_selection, "{id:?}");
            assert_eq!(id.category(), Category::View, "{id:?}");
        }
    }
}
