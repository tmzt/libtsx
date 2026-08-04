// The Default template's registry: the screens and modules this project ships.
//
// Ids are authored, never minted; the convention and the reason it matters are
// in crates/libhbui/src/layout.rs. These ids belong to the REGISTRY ENTRIES
// here, not to the screens they name - every screen file declares its own root.
//
// KNOWN: `AppDoc::manifest()` regenerates this registry without ids on load, so
// these survive the published ZIP but not an Action Store round trip.

<App id="1286e29c8e61bd9542e1e042ffffffff">
    <Screen id="a71df2175a2dcff5ebd4b91dffffffff" name="Home" icon="home" file="home.tsx" />
    <Screen id="e590be202653a1d0aeccf19dffffffff" name="Search" icon="search" file="search.tsx" />
    <Screen id="7dc2535e89cba1eeca0ec221ffffffff" name="Library" icon="library_books" file="library.tsx" />
    <Screen id="9a347b1c3b6048631ed7590cffffffff" name="Settings" icon="settings" file="settings.tsx" />
    <Module id="58196fbf0337bf137445171cffffffff" name="Library Feed" language="TypeScript" file="library-feed.ts" />
    <Module id="aaa38e509ff48eb5aed8526bffffffff" name="Search Feed" language="TypeScript" file="search-feed.ts" />
</App>
