import { JSX, splitProps } from "solid-js";

export interface CardProps extends JSX.HTMLAttributes<HTMLDivElement> {
  interactive?: boolean;
}

export function Card(props: CardProps) {
  const [local, rest] = splitProps(props, ["interactive", "class", "children"]);
  return (
    <div
      {...rest}
      class={`ui-card${local.interactive ? " ui-card-interactive" : ""}${local.class ? " " + local.class : ""}`}
    >
      {local.children}
    </div>
  );
}
