export default function Spinner(props) {
    const s = props.size ?? 40;
    return (<div class="spinner" style={{ width: `${s}px`, height: `${s}px` }}/>);
}
