// Baychat's registry: the screens, modules and widgets the project ships.
//
// Ids are authored, never minted; the convention is in
// crates/libhbui/src/layout.rs. These ids belong to the REGISTRY ENTRIES here,
// not to the definitions they name - each of those declares its own root id.
//
// No `host:effects` import: a registry entry is not a control and can fire no
// event, so importing it would name a capability this definition cannot use.
// `screens/chat.tsx` is where it is imported and used.

<App id="d9c5a6c00573f43dbfe800bfffffffff" depth="Two" entry="Chat">
    <Screen id="978b3bf446f8ad7deb7d6f48ffffffff" name="Chat" icon="chat" file="chat.tsx" />
    <Screen id="03cdb7a9096456c8af163770ffffffff" name="Profile" icon="person" file="profile.tsx" />
    <Module id="79c866c2921bfcedad6515b6ffffffff" name="Chat Feed" language="TypeScript" file="chat-feed.ts" />
    <Widget id="67dace6ce0435fcb9870beaaffffffff" name="MessageInput" icon="chat" file="message_input.tsx" />
    <Widget id="f7de2c6b9a814e53b7c0d2faffffffff" name="ChatFeedEntry" icon="chat" file="chat_feed_entry.tsx" />
</App>
