// Baychat's registry: the screens and modules the project ships.
//
// Every element carries a stored 32-hex `id` (Rules 40, 41) - 24 hex digits of
// widget identity from /dev/urandom, then `ffffffff` for "not a row". The two
// `<Screen>` ids are the ones this file has always carried and are left
// untouched: an id is stored identity and is never recomputed (Rule 2).
//
// These ids belong to the REGISTRY ENTRIES in this definition, not to the
// screen definitions they point at: `screens/chat.tsx` declares its own root id
// and `screens/profile.tsx` declares its own. Ids are unique within a
// definition, and these are four different definitions.
//
// `host:effects` is not imported here. Nothing in this file can fire an event -
// a registry entry is not a control - so an import would name a capability the
// definition cannot use. It is imported and USED by `screens/chat.tsx`.

<App id="d9c5a6c00573f43dbfe800bfffffffff" depth="Two" entry="Chat">
    <Screen id="978b3bf446f8ad7deb7d6f48ffffffff" name="Chat" icon="chat" file="chat.tsx" />
    <Screen id="03cdb7a9096456c8af163770ffffffff" name="Profile" icon="person" file="profile.tsx" />
    <Module id="79c866c2921bfcedad6515b6ffffffff" name="Chat Feed" language="TypeScript" file="chat-feed.ts" />
    <Widget id="67dace6ce0435fcb9870beaaffffffff" name="MessageInput" icon="chat" file="message_input.tsx" />
    <Widget id="f7de2c6b9a814e53b7c0d2faffffffff" name="ChatFeedEntry" icon="chat" file="chat_feed_entry.tsx" />
</App>
