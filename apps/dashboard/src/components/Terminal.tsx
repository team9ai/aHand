import { createEffect, type Component } from "solid-js";

interface TerminalProps {
  stdout: string;
  stderr: string;
}

const Terminal: Component<TerminalProps> = (props) => {
  let el!: HTMLDivElement;

  createEffect(() => {
    // Re-read props to track changes.
    props.stdout;
    props.stderr;
    // Auto-scroll to bottom.
    if (el) el.scrollTop = el.scrollHeight;
  });

  return (
    <div class="terminal" ref={el}>
      {props.stdout && <span class="stdout">{props.stdout}</span>}
      {props.stderr && <span class="stderr">{props.stderr}</span>}
    </div>
  );
};

export default Terminal;
