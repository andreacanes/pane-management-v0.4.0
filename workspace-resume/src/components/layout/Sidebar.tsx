import { createSignal, For, Show, createMemo, onMount } from "solid-js";
import { createDroppable, useDragDropContext } from "@thisbeyond/solid-dnd";
import { useApp } from "../../contexts/AppContext";
import { ProjectCard } from "../project/ProjectCard";
import { NewProjectFlow } from "../project/NewProjectFlow";
import type { ProjectTier, ProjectWithMeta } from "../../lib/types";
import { Plus, ChevronDown, ChevronRight, Search, X } from "../ui/icons";
import { SkeletonProjectCard } from "../ui/Skeleton";

type FilterOption = "all" | ProjectTier;

const FILTER_OPTIONS: { value: FilterOption; label: string }[] = [
  { value: "all", label: "All" },
  { value: "pinned", label: "Pinned" },
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
  { value: "archived", label: "Archived" },
];

/** Sections shown when the "all" filter is active. */
const TIER_SECTIONS: { tier: ProjectTier; label: string; defaultOpen: boolean }[] = [
  { tier: "pinned",   label: "Pinned",   defaultOpen: true  },
  { tier: "active",   label: "Active",   defaultOpen: true  },
  { tier: "paused",   label: "Paused",   defaultOpen: false },
  { tier: "archived", label: "Archived", defaultOpen: false },
];

function deriveName(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

export function Sidebar(props: { width?: number }) {
  const { state, projectsByTier } = useApp();
  const [filter, setFilter] = createSignal<FilterOption>("all");
  const [searchText, setSearchText] = createSignal("");
  const [collapsed, setCollapsed] = createSignal(false);
  const [showNewProject, setShowNewProject] = createSignal(false);
  const [initialLoadWindow, setInitialLoadWindow] = createSignal(true);
  onMount(() => setTimeout(() => setInitialLoadWindow(false), 1500));
  const showSkeletons = () => initialLoadWindow() && state.projects.length === 0;

  // Collapsed/expanded state for each tier section when filter === "all"
  const [openSections, setOpenSections] = createSignal<Record<ProjectTier, boolean>>({
    pinned: true,
    active: true,
    paused: false,
    archived: false,
  });

  function toggleSection(tier: ProjectTier) {
    setOpenSections({ ...openSections(), [tier]: !openSections()[tier] });
  }

  const unpinDroppable = createDroppable("sidebar-unpin");
  const dndContext = useDragDropContext();
  const isDragging = () => dndContext?.[0]?.active?.draggable != null;
  const isDraggingPin = () => {
    const id = dndContext?.[0]?.active?.draggable?.id;
    return typeof id === "string" && id.startsWith("pin:");
  };

  const searchFilter = (list: ProjectWithMeta[]): ProjectWithMeta[] => {
    const search = searchText().toLowerCase().trim();
    if (!search) return list;
    return list.filter((p) => {
      const displayName = (p.meta.display_name || "").toLowerCase();
      const folderName = deriveName(p.actual_path).toLowerCase();
      const path = p.actual_path.toLowerCase();
      return displayName.includes(search) || folderName.includes(search) || path.includes(search);
    });
  };

  /** Flat filtered list used when a specific tier chip is selected. */
  const filteredProjects = createMemo((): ProjectWithMeta[] => {
    const f = filter();
    const projects: ProjectWithMeta[] = f === "all" ? state.projects : projectsByTier(f);
    return searchFilter(projects);
  });

  /**
   * Per-tier memos. Each one returns the same stable project references
   * when state.projects is updated via `reconcile`, so SolidJS `<For>`
   * preserves ProjectCard DOM nodes (and their `createResource` usage
   * cache) across the 10s refresh poll.
   */
  const pinnedItems   = createMemo(() => searchFilter(projectsByTier("pinned")));
  const activeItems   = createMemo(() => searchFilter(projectsByTier("active")));
  const pausedItems   = createMemo(() => searchFilter(projectsByTier("paused")));
  const archivedItems = createMemo(() => searchFilter(projectsByTier("archived")));

  const anyGroupedItems = () =>
    pinnedItems().length + activeItems().length + pausedItems().length + archivedItems().length > 0;

  const tierCount = (tier: FilterOption): number =>
    tier === "all" ? state.projects.length : projectsByTier(tier).length;

  return (
    <aside
      class={`sidebar ${collapsed() ? "collapsed" : ""}`}
      style={!collapsed() && props.width ? { width: `${props.width}px`, "min-width": `${props.width}px` } : undefined}
    >
      <button
        class="sidebar-toggle"
        onClick={() => setCollapsed((v) => !v)}
        title={collapsed() ? "Expand sidebar" : "Collapse sidebar"}
      >
        {collapsed() ? "\u25B6" : "\u25C0"}
      </button>

      <Show when={!collapsed()}>
        {/* Search */}
        <div class="sidebar-search">
          <span class="sidebar-search-icon"><Search size={13} /></span>
          <input
            class="sidebar-search-input"
            type="text"
            placeholder="Search projects..."
            value={searchText()}
            onInput={(e) => setSearchText(e.currentTarget.value)}
          />
          <Show when={searchText()}>
            <button
              class="sidebar-search-clear"
              onClick={() => setSearchText("")}
              title="Clear search"
              aria-label="Clear search"
            >
              <X size={12} />
            </button>
          </Show>
        </div>

        {/* Tier filter chips */}
        <div class="sidebar-tier-chips">
          <For each={FILTER_OPTIONS}>
            {(opt) => (
              <button
                class={`sidebar-tier-chip ${filter() === opt.value ? "active" : ""}`}
                onClick={() => setFilter(opt.value)}
              >
                <span>{opt.label}</span>
                <span class="sidebar-tier-chip-count">{tierCount(opt.value)}</span>
              </button>
            )}
          </For>
        </div>

        <div class="sidebar-actions-row">
          <button
            class="new-project-btn"
            onClick={() => setShowNewProject((v) => !v)}
            title="Add a new project from any folder"
          >
            <Plus size={12} /> New project
          </button>
        </div>

        {/* New project flow */}
        <Show when={showNewProject()}>
          <NewProjectFlow onCancel={() => setShowNewProject(false)} />
        </Show>

        {/* Project list */}
        <div class="sidebar-project-list" style={{ position: "relative" }}>
          {/* Unpin droppable — always mounted so solid-dnd can register it */}
          <div
            ref={(el) => unpinDroppable(el)}
            class={`sidebar-unpin-overlay ${unpinDroppable.isActiveDroppable ? "drop-active" : ""}`}
            style={{ display: isDraggingPin() ? "flex" : "none" }}
          >
            Drag here to unpin
          </div>

          {/* First-paint skeletons while initial project list is loading */}
          <Show when={showSkeletons()} fallback={
          <Show
            when={filter() === "all"}
            fallback={
              <Show
                when={filteredProjects().length > 0}
                fallback={
                  <div class="sidebar-empty">
                    No {filter()} projects found
                  </div>
                }
              >
                <Show when={!isDragging()}>
                  <div class="sidebar-drag-hint">Drag to load in pane</div>
                </Show>
                <For each={filteredProjects()}>
                  {(project) => <ProjectCard project={project} />}
                </For>
              </Show>
            }
          >
            <Show
              when={anyGroupedItems()}
              fallback={
                <div class="sidebar-empty">
                  {searchText() ? "No projects match your search" : "No projects yet"}
                </div>
              }
            >
              <Show when={!isDragging()}>
                <div class="sidebar-drag-hint">Drag to load in pane</div>
              </Show>
              <TierSection
                label="Pinned"
                tier="pinned"
                items={pinnedItems()}
                open={openSections().pinned}
                onToggle={() => toggleSection("pinned")}
              />
              <TierSection
                label="Active"
                tier="active"
                items={activeItems()}
                open={openSections().active}
                onToggle={() => toggleSection("active")}
              />
              <TierSection
                label="Paused"
                tier="paused"
                items={pausedItems()}
                open={openSections().paused}
                onToggle={() => toggleSection("paused")}
              />
              <TierSection
                label="Archived"
                tier="archived"
                items={archivedItems()}
                open={openSections().archived}
                onToggle={() => toggleSection("archived")}
              />
            </Show>
          </Show>
          }>
            <SkeletonProjectCard />
            <SkeletonProjectCard />
            <SkeletonProjectCard />
            <SkeletonProjectCard />
          </Show>
        </div>
      </Show>
    </aside>
  );
}

/** Single tier section. Kept as a stable component so its inner <For>
 *  preserves ProjectCard identity across 10s polls. */
function TierSection(props: {
  label: string;
  tier: ProjectTier;
  items: ProjectWithMeta[];
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <Show when={props.items.length > 0}>
      <div class="sidebar-section">
        <button class="sidebar-section-header" onClick={props.onToggle}>
          {props.open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          <span class="sidebar-section-label">{props.label}</span>
          <span class="sidebar-section-count">{props.items.length}</span>
        </button>
        <Show when={props.open}>
          <div class="sidebar-section-body">
            <For each={props.items}>
              {(project) => <ProjectCard project={project} />}
            </For>
          </div>
        </Show>
      </div>
    </Show>
  );
}
