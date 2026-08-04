// Baychat's authored profile screen. The shared renderer does not synthesize
// children from a legacy SettingsView, so every visible band and every action
// is declared here and participates in the same retained layout as hit-test.

import { navigate, toggleDrawer } from "host:effects";

<Screen id="a80917dbd52ea5f824080e0affffffff" name="Profile" icon="person" section="Recent Profiles" width="fill" height="fill" direction="column" fill="surface">
    <Column id="5b1316aa8cce08143bc24ae1ffffffff" width="fill" height="fill" direction="column">
        <AppBar id="6f4b06bcbfe6391ea6740975ffffffff" width="fill" height={90} direction="row" padding={22} spacing={22} fill="surface" elevation={0}>
            <Action id="efc4d94c8a86821bf15053bdffffffff" width={36} height={36} onTap={toggleDrawer()}>
                <Icon id="0d506b2fdfcab72f032e950affffffff" name="menu" width="fill" height="fill" textSize={48} textColor="on-surface" />
            </Action>
            <Title id="02dc334e37552b78991ee646ffffffff" width="fill" height="fill" textSize={31} textColor="on-surface" truncate={true}>Profile</Title>
            <Action id="51ee26073caeb62ec95d63ccffffffff" width={36} height={36} onTap={navigate("Chat")}>
                <Icon id="a048954a059463df15c26b14ffffffff" name="chat" width="fill" height="fill" textSize={48} textColor="on-surface" />
            </Action>
        </AppBar>

        <Column id="10d113c75e8c7de9cbb255edffffffff" width="fill" height="fill" direction="column" padding={28} spacing={18}>
            <Avatar id="608a6ccaf68a5af88cc70825ffffffff" width={88} height={88} shape="circle" fill="tertiary-container" textSize={30} textColor="on-tertiary-container" textAlign="center">AV</Avatar>
            <Content id="91aaf919936a34e42d9a202cffffffff" width="fill" height={42} textSize={31} textColor="on-surface">Ari Voss</Content>
            <Content id="1a20c185398c5b5d9d865f66ffffffff" width="fill" height={30} textSize={21} textColor="primary">Online</Content>
            <Divider id="28abf3e409166f20a9ed47fdffffffff" width="fill" thickness={1} />
            <Row id="7f898c23656e1aa4d2644eb5ffffffff" width="fill" height={58} direction="row" spacing={20}>
                <Icon id="2925320ef6101ba59a0b85b7ffffffff" name="person" width={32} height={32} textSize={38} textColor="on-surface-variant" />
                <Content id="6efb0719fb12eb75f128f00dffffffff" width="fill" height="fill" textSize={22} textColor="on-surface">@arivoss</Content>
            </Row>
            <Row id="c8997b2ad09d405c22092242ffffffff" width="fill" height={58} direction="row" spacing={20}>
                <Icon id="b69c8cd0b6570a9688528809ffffffff" name="schedule" width={32} height={32} textSize={38} textColor="on-surface-variant" />
                <Content id="02cc38ff15186e1021929510ffffffff" width="fill" height="fill" textSize={22} textColor="on-surface">9:14 PM local time</Content>
            </Row>
        </Column>
    </Column>
</Screen>
