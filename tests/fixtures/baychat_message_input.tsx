// Baychat's composed message field. Interaction belongs to the TextEntry and
// Action primitives; this widget only arranges them.

import { navigate } from "host:effects";

<Widget id="d4c0825208d8f51bcf7952c0ffffffff" width="fill" height={101} direction="row" padding={23} spacing={11} fill="surface" elevation={0}>
    <TextEntry id="d9d509784f0926068c93e8b7ffffffff" width={415} height={68} direction="row" padding={22} corner={34} fill="surface-container-high" elevation={0}>
        <Content id="e4c20f457857e8255f776dabffffffff" width="fill" height="fill" textSize={23} value="d9d509784f0926068c93e8b7ffffffff" textColor="on-surface-variant" valueColor="on-surface" truncate={true}>Message #baychat-general</Content>
    </TextEntry>
    <Action id="74fbc731b247bc032c042e99ffffffff" width={68} height={68} shape="circle" fill="secondary-container" onTap={navigate("Chat")}>
        <Icon id="82a496cfb842f0c5f247579fffffffff" name="send" width="fill" height="fill" textSize={36} textColor="on-secondary-container" />
    </Action>
</Widget>
