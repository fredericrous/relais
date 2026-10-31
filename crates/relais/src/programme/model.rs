//! The values the programme modules exchange.

/// Where a ticket stands in its own file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
    OutOfScope,
}

impl Status {
    pub const NAMES: &'static str = "open, closed, out-of-scope";

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            "out-of-scope" => Some(Self::OutOfScope),
            _ => None,
        }
    }
}

/// What kind of decision a ticket resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketType {
    Research,
    Prototype,
    Grilling,
    Task,
}

impl TicketType {
    pub const NAMES: &'static str = "research, prototype, grilling, task";

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "research" => Some(Self::Research),
            "prototype" => Some(Self::Prototype),
            "grilling" => Some(Self::Grilling),
            "task" => Some(Self::Task),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Research => "research",
            Self::Prototype => "prototype",
            Self::Grilling => "grilling",
            Self::Task => "task",
        }
    }
}

/// Whether a ticket can be worked without the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Afk,
    Hitl,
}

impl Mode {
    pub const NAMES: &'static str = "afk, hitl";

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "afk" => Some(Self::Afk),
            "hitl" => Some(Self::Hitl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Afk => "afk",
            Self::Hitl => "hitl",
        }
    }
}

/// One decision ticket, as its file states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// The file stem.
    pub id: String,
    /// The `# ` heading.
    pub name: String,
    pub kind: TicketType,
    pub mode: Mode,
    pub status: Status,
    pub blocked_by: Vec<String>,
    /// `None` when the file does not say: the map's own repository.
    pub repos: Option<Vec<String>>,
    pub has_question: bool,
    pub has_resolution: bool,
    pub status_line: usize,
    pub blocked_by_line: usize,
    pub repos_line: usize,
    pub heading_line: usize,
}

/// The parts of `map.md` the commands read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapDoc {
    pub destination: String,
    pub decisions: String,
    pub out_of_scope: String,
}

/// One thing wrong, with the file (relative to the programme directory)
/// and the 1-based line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub line: usize,
    pub message: String,
}

pub const MAP_FILE: &str = "map.md";

/// The map's sections, in the order they must appear.
pub const MAP_SECTIONS: [&str; 5] = [
    "Destination",
    "Notes",
    "Decisions so far",
    "Not yet specified",
    "Out of scope",
];
