// Baychat's chat screen, in libhbui's dialect. Sized for 540 x 1158, the
// canvas the committed device review frames use.
//
// Ids, boxes and paints are REUSED from crates/libhbui/fixtures/chat.tsx rather
// than re-derived, so the two files name the same widgets and cannot drift into
// two sets of values that merely look alike.
//
// Three constraints shape this file and none of them are guessable from the
// markup, so they are written down here:
//
//  1. `<Screen>` takes exactly one child (R4, `check_screen_children`), which
//     is why a `<Column>` wraps everything.
//  2. Every `<Content>` under the screen's content child is projected as a LIST
//     ROW. That is why the app bar title is a `<Title>` - a `<Content>` there
//     would become row 0 of the thread.
//  3. Row projection is POSITIONAL over `<Content>` document order: headline,
//     supporting text, then timestamp. That order is forced, which is why the
//     timestamp sits below the bubble rather than in `<Head>`.
//
// One template cannot vary its fill per row, so every bubble is
// `surface-container-high`.

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
