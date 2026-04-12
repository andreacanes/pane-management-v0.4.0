import { JSX } from "solid-js";
import { Circle, Activity, AlertTriangle, CheckCircle2 } from "lucide-solid";

export type StatusKind = "idle" | "running" | "waiting" | "done";

const LABELS: Record<StatusKind, string> = {
  idle: "Idle",
  running: "Running",
  waiting: "Waiting",
  done: "Done",
};

const ICONS: Record<StatusKind, (p: { size: number }) => JSX.Element> = {
  idle: (p) => <Circle size={p.size} />,
  running: (p) => <Activity size={p.size} />,
  waiting: (p) => <AlertTriangle size={p.size} />,
  done: (p) => <CheckCircle2 size={p.size} />,
};

export interface StatusChipProps {
  status: StatusKind;
  label?: string;
  compact?: boolean;
  title?: string;
}

export function StatusChip(props: StatusChipProps) {
  const label = () => props.label ?? LABELS[props.status];
  const size = () => (props.compact ? 10 : 12);
  const Icon = () => ICONS[props.status]({ size: size() });
  return (
    <span
      class={`ui-status-chip ui-status-${props.status}${props.compact ? " ui-status-compact" : ""}`}
      title={props.title}
    >
      <span class="ui-status-icon">{Icon()}</span>
      {!props.compact && <span class="ui-status-label">{label()}</span>}
    </span>
  );
}
