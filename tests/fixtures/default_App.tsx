// The Default template's registry: the screens and modules this project ships.
//
// Every element carries a stored 32-hex `id` (Rules 40, 41) - 24 hex digits of
// widget identity from /dev/urandom, then `ffffffff`, which is `u32::MAX` in
// the row half and means "not a row" (`...00000000` is row ZERO and is refused
// as `DeclaredIdIsARow`). They are AUTHORED, never derived and never minted at
// build time: minting hands a widget fresh identity on every rebuild, which is
// the defect class `HbWidgetId` exists to remove (crates/libhbui/src/layout.rs).
//
// These ids belong to the REGISTRY ENTRIES in this definition, not to the
// screen definitions they point at: `screens/home.tsx` declares its own root id
// and so does every other screen file. Those are five different definitions.
//
// NOTE (finding, not a defect of this file): `AppDoc` does not retain this
// file - `AppDoc::manifest()` REGENERATES the registry from the screen/widget/
// module sets on every load, without ids, and
// `libteststand::memory_package::give_app_registry_identity` then mints one for
// the regenerated `<App>`. So the ids below survive the ZIP but not the
// Action Store round trip. They are authored anyway, because the file is the
// authored form and the loader is what should learn to read them.

<App id="1286e29c8e61bd9542e1e042ffffffff">
    <Screen id="a71df2175a2dcff5ebd4b91dffffffff" name="Home" icon="home" file="home.tsx" />
    <Screen id="e590be202653a1d0aeccf19dffffffff" name="Search" icon="search" file="search.tsx" />
    <Screen id="7dc2535e89cba1eeca0ec221ffffffff" name="Library" icon="library_books" file="library.tsx" />
    <Screen id="9a347b1c3b6048631ed7590cffffffff" name="Settings" icon="settings" file="settings.tsx" />
    <Module id="58196fbf0337bf137445171cffffffff" name="Library Feed" language="TypeScript" file="library-feed.ts" />
    <Module id="aaa38e509ff48eb5aed8526bffffffff" name="Search Feed" language="TypeScript" file="search-feed.ts" />
</App>
