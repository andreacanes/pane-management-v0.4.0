import { JSX, splitProps } from "solid-js";

type Tone = "neutral" | "accent" | "success" | "warning" | "danger";

export interface BadgeProps extends JSX.HTMLAttributes<HTMLSpanElement> {
  tone?: Tone;
}

export function Badge(props: BadgeProps) {
  const [local, rest] = splitProps(props, ["tone", "class", "children"]);
  const tone = () => local.tone ?? "neutral";
  return (
    <span
      {...rest}
      class={`ui-badge ui-badge-${tone()}${local.class ? " " + local.class : ""}`}
    >
      {local.children}
    </span>
  );
}
