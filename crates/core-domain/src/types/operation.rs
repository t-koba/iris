#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TicketId(pub u64);

/// Ticket for tracking operations that do not complete immediately, such as server-side copy.
#[derive(Clone, Debug)]
pub struct OperationTicket {
    pub id: TicketId,
    pub kind: OperationKind,
}

#[derive(Clone, Debug)]
pub enum OperationKind {
    Copy,
    Move,
    Delete,
}
