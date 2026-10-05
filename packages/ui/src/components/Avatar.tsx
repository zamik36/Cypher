import { avatarColour, initials } from "../utils/avatar";

/** A contact's initials on their stable colour. */
export default function Avatar(props: { peerId: string; name: string; size?: number }) {
  const size = () => props.size ?? 40;
  return (
    <span
      class="avatar"
      data-colour={avatarColour(props.peerId)}
      style={{ width: `${size()}px`, height: `${size()}px`, "font-size": `${Math.round(size() * 0.4)}px` }}
      aria-hidden="true"
    >
      {initials(props.name) || "?"}
    </span>
  );
}
