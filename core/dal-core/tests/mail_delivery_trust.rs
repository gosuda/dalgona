//! Mail delivery trust boundary, proven through the actual record codec
//! and replay: a `mail_delivered` line is the whole delivery, so the fold
//! must own what it may claim. A forged text never reaches turn input, a
//! refused line consumes nothing, and the waiting-mail bound counts only
//! this recipient's undelivered `NextTurn` rows, never history.

use std::num::NonZeroU64;

use dal_core::{
    DeliveredMail, Entry, EntryId, EntryKind, Header, JournalPart, Mail, MailMode, Product, Record,
    Session, SessionId, Timestamp, TurnId, Workspace, decode, encode,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn turn_id(value: u64) -> Result<TurnId, Box<dyn std::error::Error>> {
    Ok(TurnId::new(
        NonZeroU64::new(value).ok_or("turn id must be nonzero")?,
    ))
}

fn entry_id(value: u64) -> Result<EntryId, Box<dyn std::error::Error>> {
    Ok(EntryId::new(
        NonZeroU64::new(value).ok_or("entry id must be nonzero")?,
    ))
}

fn stamp() -> Timestamp {
    Timestamp::UNIX_EPOCH
}

fn tomorrow() -> Result<Timestamp, Box<dyn std::error::Error>> {
    Ok(Timestamp::from_second(86_400)?)
}

fn recipient() -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok(SessionId::parse("01890f47-36b0-7cc4-8000-000000000001")?)
}

fn workspace() -> Result<Workspace, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    let root = std::path::PathBuf::from("/");
    #[cfg(windows)]
    let root = std::path::PathBuf::from("C:\\");
    Ok(Workspace::new(root)?)
}

fn header(session_id: SessionId) -> Result<Record, Box<dyn std::error::Error>> {
    Ok(Record::Session(Header {
        id: session_id,
        at: stamp(),
        workspace: workspace()?,
        product: Product::Dal,
        from: None,
    }))
}

fn mail_row(to: SessionId, mode: MailMode, text: &str) -> Record {
    Record::Mail(Mail {
        at: stamp(),
        from: SessionId::new_v7(),
        to,
        mode,
        text: text.into(),
        reply_to: None,
    })
}

fn user_entry(value: u64, text: &str) -> Result<Record, Box<dyn std::error::Error>> {
    Ok(Record::User(Entry {
        id: entry_id(value)?,
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }))
}

fn delivered(position: u64, text: &str) -> Result<DeliveredMail, Box<dyn std::error::Error>> {
    Ok(DeliveredMail {
        position: entry_id(position)?,
        text: text.into(),
    })
}

fn delivery(
    at: Timestamp,
    entry: u64,
    mails: Vec<DeliveredMail>,
) -> Result<Record, Box<dyn std::error::Error>> {
    Ok(Record::MailDelivered {
        at,
        turn: turn_id(1)?,
        entry: entry_id(entry)?,
        mails,
    })
}

#[test]
fn hundred_waiting_mails_deliver_and_reread_identically() -> TestResult {
    let to = recipient()?;
    let mut journal = vec![header(to)?];
    for index in 1..=100 {
        journal.push(mail_row(to, MailMode::NextTurn, &format!("mail-{index}")));
    }
    let waiting = Session::replay_declared(journal.iter().cloned()).unwrap();
    assert_eq!(waiting.pending_mails(), 100);
    assert_eq!(waiting.next_mail_position(), Some(entry_id(101)?));

    journal.extend(open_turn_records()?);
    let mails: Vec<DeliveredMail> = (1..=100)
        .map(|index| delivered(index, &format!("mail-{index}")))
        .collect::<Result<Vec<_>, _>>()?;
    journal.push(delivery(tomorrow()?, 2, mails)?);

    let replayed = Session::replay_declared(journal.iter().cloned()).unwrap();
    assert_eq!(replayed.pending_mails(), 0);
    assert_eq!(
        replayed.next_mail_position(),
        Some(entry_id(101)?),
        "delivered history keeps the row count"
    );

    // The actual codec carries the journal, and a read replays the exact
    // same state: encode every record, decode it back, fold again.
    let reread: Vec<Record> = journal
        .iter()
        .map(|record| decode(&encode(record).unwrap()).unwrap().record)
        .collect();
    assert_eq!(Session::replay_declared(reread).unwrap(), replayed);

    // Capacity frees by delivery, not erasure: new waiting rows follow
    // the 100 delivered ones.
    for index in 101..=102 {
        journal.push(mail_row(to, MailMode::NextTurn, &format!("late-{index}")));
    }
    let refilled = Session::replay_declared(journal).unwrap();
    assert_eq!(refilled.pending_mails(), 2);
    assert_eq!(refilled.next_mail_position(), Some(entry_id(103)?));

    Ok(())
}

fn open_turn_records() -> Result<Vec<Record>, Box<dyn std::error::Error>> {
    Ok(vec![
        Record::TurnReserved {
            at: stamp(),
            turn: turn_id(1)?,
        },
        Record::TurnStart {
            at: stamp(),
            turn: turn_id(1)?,
        },
        user_entry(1, "question")?,
    ])
}

#[test]
fn forged_delivery_text_replays_nothing() -> TestResult {
    let to = recipient()?;
    let mut prefix = vec![header(to)?];
    prefix.push(mail_row(to, MailMode::NextTurn, "one"));
    prefix.push(mail_row(to, MailMode::NextTurn, "two"));
    prefix.extend(open_turn_records()?);

    // A delivery line whose text diverges from the retained row is a
    // contradiction, whether the forgery rides the first mail or a later
    // one behind honest mail.
    let mut forged = prefix.clone();
    forged.push(delivery(
        tomorrow()?,
        2,
        vec![delivered(1, "one")?, delivered(2, "one-forged")?],
    )?);
    assert!(
        Session::replay_declared(forged.iter().cloned()).is_err(),
        "a forged text never reaches turn input"
    );
    let decoded: Vec<Record> = forged
        .iter()
        .map(|record| decode(&encode(record).unwrap()).unwrap().record)
        .collect();
    assert!(
        Session::replay_declared(decoded).is_err(),
        "the fold, not the writer, owns what a delivery line may claim"
    );

    // A bad line consumes nothing: the honest delivery of the same
    // position still replays, because the forgery left it waiting.
    let mut honest = prefix.clone();
    honest.push(delivery(tomorrow()?, 2, vec![delivered(1, "one")?])?);
    let delivered_one = Session::replay_declared(honest).unwrap();
    assert_eq!(delivered_one.pending_mails(), 1);
    assert_eq!(delivered_one.next_mail_position(), Some(entry_id(3)?));

    let mut single = prefix.clone();
    single.push(delivery(tomorrow()?, 2, vec![delivered(1, "not-one")?])?);
    assert!(Session::replay_declared(single).is_err());

    let mut trailing = prefix;
    trailing.push(delivery(
        tomorrow()?,
        2,
        vec![delivered(1, "one")?, delivered(9, "never-waiting")?],
    )?);
    assert!(Session::replay_declared(trailing).is_err());

    Ok(())
}

#[test]
fn delivery_repeats_reuse_and_wrong_turn_stay_contradictions() -> TestResult {
    let to = recipient()?;
    let mut journal = vec![header(to)?];
    journal.push(mail_row(to, MailMode::NextTurn, "one"));
    journal.push(mail_row(to, MailMode::NextTurn, "two"));
    journal.extend(open_turn_records()?);

    // The same position twice in one line consumes nothing: validation
    // runs before the first occurrence could land.
    let mut repeated = journal.clone();
    repeated.push(delivery(
        tomorrow()?,
        2,
        vec![delivered(1, "one")?, delivered(1, "one")?],
    )?);
    assert!(Session::replay_declared(repeated).is_err());

    // A delivery reusing a tree entry, and a delivery naming a turn other
    // than the open one, stay contradictions.
    let mut reused = journal.clone();
    reused.push(delivery(tomorrow()?, 1, vec![delivered(1, "one")?])?);
    assert!(Session::replay_declared(reused).is_err());

    journal.push(Record::MailDelivered {
        at: tomorrow()?,
        turn: turn_id(2)?,
        entry: entry_id(2)?,
        mails: vec![delivered(1, "one")?],
    });
    assert!(Session::replay_declared(journal).is_err());

    Ok(())
}

#[test]
fn waiting_limit_counts_undelivered_next_turn_rows_only() -> TestResult {
    let to = recipient()?;

    // The 101st waiting row for one recipient is a contradiction, and a
    // refused line leaves the journal exactly as the first 100 wrote it.
    let mut overflow = vec![header(to)?];
    for index in 1..=101 {
        overflow.push(mail_row(to, MailMode::NextTurn, &format!("mail-{index}")));
    }
    assert!(Session::replay_declared(overflow).is_err());

    // Exactly 100 wait, and rows that never join the pending state are
    // never capped: `Aside` mode and a foreign recipient stay readable
    // beside a full mailbox.
    let mut at_limit = vec![header(to)?];
    for index in 1..=100 {
        at_limit.push(mail_row(to, MailMode::NextTurn, &format!("mail-{index}")));
    }
    at_limit.push(mail_row(to, MailMode::Aside, "aside"));
    at_limit.push(mail_row(SessionId::new_v7(), MailMode::NextTurn, "foreign"));
    let held = Session::replay_declared(at_limit).unwrap();
    assert_eq!(held.pending_mails(), 100);
    assert_eq!(held.next_mail_position(), Some(entry_id(103)?));

    Ok(())
}
