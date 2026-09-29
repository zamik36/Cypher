export default function Spinner(props: { size?: number }) {
  const size = () => `${props.size ?? 40}px`;
  return (
    <div
      class="spinner"
      style={{ width: size(), height: size() }}
    />
  );
}
