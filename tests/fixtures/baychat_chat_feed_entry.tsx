// Baychat's bound chat-feed row. The screen owns the List and feed binding;
// this widget owns the presentation of one Message record.

<Widget id="f2c6a2e7d4b94681a0e5c39fffffffff" width="fill" height={135} direction="row" spacing={23}>
    <Avatar id="ceace6b6411642d181218013ffffffff" width={56} height={56} shape="circle" fill="tertiary-container" textSize={17} textColor="on-tertiary-container" textAlign="center" authoringPlaceholder="A">{"{{avatar}}"}</Avatar>
    <Column id="dc46a69a2b15514a2bbf7a3cffffffff" width="fill" height="fill" direction="column" spacing={6}>
        <Head id="9d5af1183ab50106cecdc522ffffffff" width="fill" height={31} direction="row" spacing={16}>
            <Content id="d6a75cd5fca14b9bb88b5049ffffffff" width="fill" height="fill" textSize={24} textColor="on-surface" truncate={true}>{"{{sender}}"}</Content>
        </Head>
        <Bubble id="bfa6e8d2d1172536678df05cffffffff" width="fill" height={70} corner={22} padding={17} fill="surface-container-high" elevation={0}>
            <Content id="5396185bde5ae77a15c241ebffffffff" width="fill" height="fill" textSize={22} textColor="on-surface" truncate={true}>{"{{text}}"}</Content>
        </Bubble>
        <Content id="9806bb8d3a9a141eda79eedfffffffff" width="fill" height={20} textSize={17} textColor="on-surface-variant" truncate={true}>{"{{time}}"}</Content>
    </Column>
</Widget>
