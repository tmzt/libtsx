// Baychat's chat screen, in libhbui's dialect.
//
// PORTED from crates/libhbui/fixtures/chat.tsx, which is the screen that
// already renders correctly (hbui-baychat-chat.png). Every id, box and paint
// value below is the fixture's, reused rather than re-derived, so the two files
// name the same widgets and cannot drift into two sets of values that merely
// look alike. Where this file differs from the fixture it is because the
// SHIPPED project requires it, and every such difference is listed at the foot
// of this comment.
//
// Every element carries a stored 32-hex `id` (Rules 40, 41): 24 hex digits of
// widget identity, then `ffffffff`, which is `u32::MAX` in the row half and
// means "not a row" (`...00000000` is row ZERO and is refused as
// `DeclaredIdIsARow`). They are authored and never derived - an ordinal is
// position-derived identity, and small values sit in the low-entropy region
// where the instance-derivation mixer was shown to collapse two widgets into
// one.
//
// Every box and every fill is DECLARED, because libhbui infers nothing from a
// tag name (Rule 30): `<Item>` is not a row unless it says `direction="row"`,
// and `<Avatar>` is not a circle unless it says `shape="circle"`. The
// vocabularies are `libhbui::layout::AttrBoxes` and `libhbui::draw::AttrPaints`.
//
// Sized for 540 x 1158 - the canvas the committed device review frames use.
//
// ASCII only (Rule 39): the baked atlas is ASCII Roboto plus a Material Symbols
// icon subset, and an em-dash renders as an invisible space.
//
// WHAT DIFFERS FROM THE FIXTURE, AND WHY
//
//  1. The six literal `<Row>`s become ONE `<ChatFeedEntry>` template under
//     `<List<Message> value={chatFeed} window={24}>`. The fixture is a
//     hand-written transcript; this is the bound list the project ships. The
//     `<Item>` remains the repeated row template and its arrow child passes
//     `item` to the composed entry. The entry's presentation lives in
//     `widgets/chat_feed_entry.tsx`.
//  2. The fixture's `<Thread>` IS this `<List>` - the list has to be the
//     screen's own content child or `AppModel` never resolves the binding - so
//     the thread's id and its box/paint ride on the `<List>`.
//  3. `<Screen>` takes exactly one child (R4, `check_screen_children`), so a
//     `<Column>` wraps what the fixture hangs directly off the screen. It is the
//     one element here with no fixture counterpart and therefore a fresh id.
//  4. The app bar title is a `<Title>`, not the fixture's `<Content>`. Every
//     `<Content>` under a screen's content child is projected as a LIST ROW, so
//     a `<Content>` in the app bar would make "Chat" row 0 of the thread.
//  5. The row's timestamp moved OUT of `<Head>` and below the bubble. The row
//     projection is positional over `<Content>` document order - first is the
//     headline, second the supporting text, third the timestamp - so
//     sender/text/time is forced, while the fixture's head row reads
//     sender/time/text.
//  6. The bubble is always `surface-container-high`. The fixture paints "You"
//     bubbles `primary`; one template cannot vary its fill per row.
//  7. The composer is a `<MessageInput>` widget call. Its TSX definition owns
//     the fixture's nested `<TextEntry>` and circular send `<Action>`; libhbui
//     expands that stored definition before layout, so input lands on the
//     primitive field rather than on the composition boundary.
//  8. `onTap={navigate("Profile")}` hangs on a new `<Action>` wrapping the app
//     bar's trailing icon, because the fixture's only `<Action>` was the send
//     button dropped in (7). It is the second element here with a fresh id.

import { navigate, toggleDrawer } from "host:effects";
import { chatFeed } from "Chat Feed";

// A message, declared (ACTIONABLE_STEPS P3/R7). `avatar` is part of the row
// even though no `<Content>` below templates it, and the reason is structural
// as well as semantic: the monogram varies per record rather than per template,
// AND a `<Content>` for it would have to sit inside the `<Avatar>`, which is the
// row's first element - so it would take the headline slot of the positional
// projection noted in (5) above and every row's headline would become its
// monogram. The field is read straight off the record instead.
interface Message {
    sender: string;
    avatar: string;
    text: string;
    time: string;
}

<Screen id="855b5251eca3124863ccec68ffffffff" name="Chat" icon="chat" section="Chats" width="fill" height="fill" direction="column" fill="surface">
    <Column id="f997cbbfb628b8fd84fdb630ffffffff" width="fill" height="fill" direction="column">

        <AppBar id="d86e43819833a0813439c4daffffffff" width="fill" height={90} direction="row" padding={22} spacing={22} fill="surface" elevation={0}>
            <Action id="aa3462f6d6d747e45ce78a51ffffffff" width={36} height={36} onTap={toggleDrawer()}>
                <Icon id="22223b2884cbc8f1787be43cffffffff" name="menu" width="fill" height="fill" textSize={48} textColor="on-surface" />
            </Action>
            <Title id="72274a4c61fff11f2e064227ffffffff" width="fill" height="fill" textSize={31} textColor="on-surface" truncate={true}>Chat</Title>
            <Action id="f66818a2492c088c8817fd6effffffff" width={36} height={36} onTap={navigate("Profile")}>
                <Icon id="75c754393cabad04ed4535b8ffffffff" name="person" width="fill" height="fill" textSize={48} textColor="on-surface" />
            </Action>
        </AppBar>

        <Scroll id="4c8d9e102a3b4c5d6e7f8091ffffffff" width="fill" height="fill" direction="column" clip={true}>
        <List<Message> id="1f1b5dd9a63bff35c8dea2c0ffffffff" value={chatFeed} window={24} width="fill" height="fill" direction="column" padding={23} spacing={14}>
            <Item id="73f8f81e6c31d413ff73b6faffffffff">
                {(item: Message) => (
                    <ChatFeedEntry id="4a63e8c72f914b5a9d0c1e6fffffffff" item={item} />
                )}
            </Item>
        </List>
        </Scroll>

        <MessageInput id="493f475cf3cf677dbdbf3e1affffffff" width="fill" height={101} />
    </Column>
</Screen>
