import type { MessageStatus } from "../platform";
import Icon, { type IconName } from "./Icon";
import { t } from "../i18n";

const ICON: Record<MessageStatus, IconName> = {
  pending: "clock",
  sent: "check",
  queued: "check",
  delivered: "check-check",
  read: "check-check",
  failed: "alert",
};

/** Delivery state of an outgoing message: clock, one tick, two, or two in the accent once read. */
export default function StatusTick(props: { status: MessageStatus }) {
  const label = () => {
    const tr = t();
    return {
      pending: tr.status_pending,
      sent: tr.status_sent,
      queued: tr.status_queued,
      delivered: tr.status_delivered,
      read: tr.status_read,
      failed: tr.status_failed,
    }[props.status];
  };
  return (
    <span class="tick" data-testid="message-status" data-status={props.status} title={label()}>
      <Icon name={ICON[props.status]} size={16} label={label()} />
    </span>
  );
}
