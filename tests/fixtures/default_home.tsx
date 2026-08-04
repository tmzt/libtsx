// The screen's PROPS shape, declared (ACTIONABLE_STEPS R7b). `<Screen<HomeProps>>`
// is a reference to this declaration - the same type parameter `<List<LibraryItem>>`
// uses for a row type, in the props position, the way a React author already
// writes a component's props (`Component<HomeProps>`).
//
// It is what `props.user` below is: the one thing this screen is parameterized
// by. Nothing here is decorative, and that is the point - until this was
// declarable, the IDE's property sheet drew `title`/`icon`/`layout`/`items` on
// every screen of every app because it had nothing to read and made a shape up.
//
// Every element carries a stored 32-hex `id` (Rules 40, 41): 24 hex digits of
// widget identity, then `ffffffff`, which is `u32::MAX` in the row half and
// means "not a row" (`...00000000` is row ZERO and is refused as
// `DeclaredIdIsARow`). Authored, never minted at build time - a widget handed
// fresh identity on every rebuild is the defect `HbWidgetId` exists to remove.
// Without them `libteststand::memory_package` cannot carry this screen at all,
// and the IDE's preview panes render nothing.
//
// These are the SAME ids `crates/highbay_data/src/fixtures/app.tsx` carries for
// this screen, reused rather than re-derived: the combined fixture and this
// per-file template are two encodings of one app, `--dump` compares the two
// projections for equality, and two sets of ids that merely looked alike would
// make them two different apps.
interface HomeProps {
    user: { name: string; email: string };
}

<Screen<HomeProps> id="df4f0d83b290486eabc1aa00ffffffff" name="Home" icon="home">
    <Column id="0ab4573ac9c1a634d739ecddffffffff">
        <UserCard id="d293b0561ef16442397fd4e8ffffffff" name={props.user.name} email={props.user.email} />
        <Item id="ae771c6d0980110c2ddea556ffffffff">
            <Content id="4fa2e1f6661e45a2534a3e21ffffffff">{"Good morning, Tim"}</Content>
            <Content id="fc537a37d0b92f47139c16f4ffffffff">{"You have 3 tasks due today"}</Content>
        </Item>
        <Item id="fc3d3207413764f5b302de70ffffffff">
            <Content id="78deb006e50b72fbff5b9701ffffffff">{"Continue reading"}</Content>
            <Content id="ac97ebda1530c863a2d4504cffffffff">{"Material 3 design guidelines"}</Content>
        </Item>
        <Item id="b2dcfbb15ef4def648b8c041ffffffff">
            <Content id="51769de76b55ba9b8970a4beffffffff">{"Weekly summary"}</Content>
            <Content id="262ece4686a98b26839ef9cbffffffff">{"Your activity is up 12 percent"}</Content>
        </Item>
        <Item id="8049daca89f25e6f0a7c635dffffffff">
            <Content id="da45084e0ea9a4c7faa639a4ffffffff">{"Storage"}</Content>
            <Content id="9926ac2251227b9258643241ffffffff">{"18.2 GB of 32 GB used"}</Content>
        </Item>
        <Item id="ec276829f1ce6eebbf7a0667ffffffff">
            <Content id="61dd02e8acef7ac949dbcbf0ffffffff">{"Backups"}</Content>
            <Content id="4fd3c5be9f6109d1e409e18affffffff">{"Last backup 2 hours ago"}</Content>
        </Item>
    </Column>
</Screen>
