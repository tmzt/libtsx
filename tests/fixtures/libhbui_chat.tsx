// Baychat's chat screen, authored in libhbui's dialect.
//
// GENERATED once and committed as source (the generator lives in the wave's
// scratchpad; regenerating it is not part of any build). It is authored
// rather than derived because libhbui reads only what an element DECLARES:
// every id is a stored 32-hex widget id (Rules 40, 41), every box comes from
// the `AttrBoxes` vocabulary and every fill, radius and type size from the
// `AttrPaints` one. Nothing is inferred from a tag name (Rule 30), which is
// why <Bubble> and <Avatar> below have to say what they paint as.
//
// It exists as EVIDENCE, in the sense CAPABILITY_REVIEW.md means: the
// capability is `draw` plus the declared paint vocabulary, and this is the
// first screen written in that way. Compare with
// crates/highbay_ui/fixtures/review/device-chrome-off-baychat-chat.png,
// which is the same screen drawn by libteststand's renderer.
//
// Sized for 540 x 1158 - half the S23 panel, at half its density, which is
// the canvas the committed device review frames use.
//
// ASCII only (Rule 39): the baked atlas is ASCII Roboto plus a Material
// Symbols icon subset, and an em-dash renders as an invisible space.

import { navigate } from "host:effects";

<Screen id="855b5251eca3124863ccec68ffffffff" width="fill" height="fill" direction="column" fill="surface">

    <Column id="7d8c67623f7e46a89fc15baeffffffff" width="fill" height="fill" direction="column" fill="surface">

    <AppBar id="d86e43819833a0813439c4daffffffff" width="fill" height={90} direction="row" padding={22} spacing={22} fill="surface" elevation={0}>
        <Icon id="22223b2884cbc8f1787be43cffffffff" name="menu" width={36} height={36} textSize={48} textColor="on-surface" />
        <Content id="72274a4c61fff11f2e064227ffffffff" width="fill" height="fill" textSize={31} textColor="on-surface" truncate={true}>Chat</Content>
        <Icon id="75c754393cabad04ed4535b8ffffffff" name="more_vert" width={36} height={36} textSize={48} textColor="on-surface" />
    </AppBar>

    <Thread id="1f1b5dd9a63bff35c8dea2c0ffffffff" width="fill" height="fill" direction="column" padding={23} spacing={14} clip={true}>

        <Row id="73f8f81e6c31d413ff73b6faffffffff" width="fill" height={135} direction="row" spacing={23}>
            <Avatar id="ceace6b6411642d181218013ffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center" authoringPlaceholder="A">YO</Avatar>
            <Column id="dc46a69a2b15514a2bbf7a3cffffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="9d5af1183ab50106cecdc522ffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="d6a75cd5fca14b9bb88b5049ffffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>You</Content>
                    <Content id="9806bb8d3a9a141eda79eedfffffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:07 PM</Content>
                </Head>
                <Bubble id="bfa6e8d2d1172536678df05cffffffff" width="fill" height={70} corner={22} padding={17} fill="primary" elevation={0}>
                    <Content id="5396185bde5ae77a15c241ebffffffff" width="fill" height="fill" textSize={22} textColor="on-primary" truncate={true}>Pushed the drawer sections patch, taking the hit path with it.</Content>
                </Bubble>
            </Column>
        </Row>

        <Row id="4c72cb4b4f593d55d9647872ffffffff" width="fill" height={135} direction="row" spacing={23}>
            <Avatar id="c86d0bc16b89390f20c43c4effffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center">AV</Avatar>
            <Column id="647d9d23d1040631828740a2ffffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="b7115ba4fc447c549f35fc41ffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="710ee34797ba7ddef0f6645affffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>Ari Voss</Content>
                    <Content id="929b942830a5f92419799d18ffffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:14 PM</Content>
                </Head>
                <Bubble id="20fa01e9019e6cc57e822dccffffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
                    <Content id="cf211c95cb81851e149f8aa1ffffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>On it - the divider between groups reads as a row today.</Content>
                </Bubble>
            </Column>
        </Row>

        <Row id="ebbd5e69ae962fe149de1651ffffffff" width="fill" height={211} direction="row" spacing={23}>
            <Avatar id="c7cfdbef4e8093d9ea11ad6dffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center">DC</Avatar>
            <Column id="4714cece88d6437a726321b7ffffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="ff207d8e1d23e42b4986303fffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="7d391e5b66b8f2148e5403d3ffffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>Devon Cole</Content>
                    <Content id="60c3df8133462f008b11f401ffffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:21 PM</Content>
                </Head>
                <Bubble id="adb6460f32d907f9b1672947ffffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
                    <Content id="f7c3c72d0565eee59908b4a2ffffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>Nice! Does the message list scroll yet, or is that next?</Content>
                </Bubble>
                <Bubble id="8d156b0339af3521ca9ba37affffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
                    <Content id="35f946f332a842e406375a4effffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>Never mind, saw the gap list. Makes sense.</Content>
                </Bubble>
            </Column>
        </Row>

        <Row id="24f918f163f0f39179b618b8ffffffff" width="fill" height={135} direction="row" spacing={23}>
            <Avatar id="042ef5da33f372198b93c91bffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center">PS</Avatar>
            <Column id="d019dd4de9530bb2d14a24f3ffffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="b77776d7ea48a1f7e8a945fcffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="4b6c4e0e1f9b762104c64c84ffffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>Priya Shah</Content>
                    <Content id="f2868fbfa2b5e5d9e3e7cdf5ffffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:35 PM</Content>
                </Head>
                <Bubble id="09c44a28ca0620eaa14448d0ffffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
                    <Content id="4682fed9b2acae1c1d774f31ffffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>Avatars are just monogram circles for now.</Content>
                </Bubble>
            </Column>
        </Row>

        <Row id="a338303eb79cfbfaac8f9052ffffffff" width="fill" height={135} direction="row" spacing={23}>
            <Avatar id="46831a8d580954d3c690d62bffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center">YO</Avatar>
            <Column id="42c35dcebb6fe2c876572bf3ffffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="d7ca402dc6cee1fef1ea4b76ffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="6d18a9851b025bcd3d7abb0affffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>You</Content>
                    <Content id="e1b35f0ea3dc0b0c61e48f69ffffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:42 PM</Content>
                </Head>
                <Bubble id="cfea9883e82bdfefd827abfdffffffff" width="fill" height={70} corner={22} padding={17} fill="primary" elevation={0}>
                    <Content id="a1592e4032fee0af80371c68ffffffff" width="fill" height="fill" textSize={22} textColor="on-primary" truncate={true}>Pushed the drawer sections patch, taking the hit path with it.</Content>
                </Bubble>
            </Column>
        </Row>

        <Row id="1b2e59276570a449f82ac2c1ffffffff" width="fill" height={135} direction="row" spacing={23}>
            <Avatar id="2f3b1d28536a9c089b83592bffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center">AV</Avatar>
            <Column id="48349138d8d05fbc31d7636effffffff" width="fill" height="fill" direction="column" spacing={6}>
                <Head id="d68ba7daaab9c0691fc328f9ffffffff" width="fill" height={31} direction="row" spacing={16}>
                    <Content id="f42f79ebd13ebd0de8feef2dffffffff" width={130} height="fill" textSize={24} textColor="on-surface" truncate={true}>Ari Voss</Content>
                    <Content id="23ca7b3dc77825ecf978639effffffff" width="fill" height="fill" textSize={17} textColor="on-surface-variant" truncate={true}>8:49 PM</Content>
                </Head>
                <Bubble id="5cbccd33086da969235f742dffffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
                    <Content id="bbd1dd8d53ac2781f492f7e1ffffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>Reading the PNG is the gate, not the green suite.</Content>
                </Bubble>
            </Column>
        </Row>
    </Thread>

    <Composer id="aa12b444f8fa0e2f741c981dffffffff" width="fill" height={101} direction="row" padding={23} spacing={11} fill="surface" elevation={0}>
        <TextEntry id="d9d509784f0926068c93e8b7ffffffff" width="fill" height={68} direction="row" padding={22} corner={34} fill="surface-container-high" elevation={0}>
            <Content id="e4c20f457857e8255f776dabffffffff" width="fill" height="fill" textSize={23} value="d9d509784f0926068c93e8b7ffffffff" textColor="on-surface-variant" valueColor="on-surface" truncate={true}>Message #baychat-general</Content>
        </TextEntry>
        <Action id="74fbc731b247bc032c042e99ffffffff" width={68} height={68} shape="circle" fill="secondary-container" onTap={navigate("Chat")}>
            <Icon id="82a496cfb842f0c5f247579fffffffff" name="send" width="fill" height="fill" textSize={36} textColor="on-secondary-container" />
        </Action>
    </Composer>

    </Column>

</Screen>
