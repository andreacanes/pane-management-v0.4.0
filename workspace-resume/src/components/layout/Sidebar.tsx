import { createSignal, For, Show, createMemo, onMount } from "solid-js";
import { useApp } from "../../contexts/AppContext";
import { ProjectCard } from "../project/ProjectCard";
import { NewProjectFlow } from "../project/NewProjectFlow";
import type { ProjectTier, ProjectWithMeta } from "../../lib/types";
import { deriveName } from "../../lib/path";
import { Plus, Search, X } from "../ui/icons";
import { SkeletonProjectCard } from "../ui/Skeleton";

type FilterOption = "all" | "pinned" | "active" | "paused";

const FILTER_OPTIONS: { value: FilterOption; label: string }[] = [
  { value: "all",    label: "All" },
  { value: "pinned", label: "Pinned" },
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
];

export function Sidebar(props: { width?: number }) {
  const { state, projectsByTier, isProjectActive, findProjectWindow, selectTmuxSession, selectTmuxWindow, openProjectSettings } = useApp();
  const [filter, setFilter] = createSignal<FilterOption>("all");
  const [searchText, setSearchText] = createSignal("");
  const [collapsed, setCollapsed] = createSignal(false);
  const [showNewProject, setShowNewProject] = createSignal(false);
  const [initialLoadWindow, setInitialLoadWindow] = createSignal(true);
  onMount(() => setTimeout(() => setInitialLoadWindow(false), 1500));
  const showSkeletons = () => initialLoadWindow() && state.projects.length === 0;

  const filteredProjects = createMemo((): ProjectWithMeta[] => {
    const f = filter();
    const all: ProjectWithMeta[] = f === "all" ? state.projects : projectsByTier(f);
    const search = searchText().toLowerCase().trim();
    if (!search) return all;
    return all.filter((p) => {
      const displayName = (p.meta.display_name || "").toLowerCase();
      const folderName = deriveName(p.actual_path).toLowerCase();
      const path = p.actual_path.toLowerCase();
      return displayName.includes(search) || folderName.includes(search) || path.includes(search);
    });
  });

  const tierCount = (tier: FilterOption): number =>
    tier === "all" ? state.projects.length : projectsByTier(tier).length;

  /** Click a project row: if it has a running pane, navigate there. Otherwise open settings. */
  function handleProjectClick(project: ProjectWithMeta) {
    const winIdx = findProjectWindow(project.encoded_name);
    if (winIdx != null) {
      selectTmuxWindow(winIdx);
    } else {
      openProjectSettings(project.encoded_name);
    }
  }

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
            >
              <X size={12} />
            </button>
          </Show>
        </div>

        {/* Tier filter chips — simplified, no Archived visible */}
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

        <Show when={showNewProject()}>
          <NewProjectFlow onCancel={() => setShowNewProject(false)} />
        </Show>

        {/* Project list */}
        <div class="sidebar-project-list">
          <Show when={showSkeletons()} fallback={
            <Show
              when={filteredProjects().length > 0}
              fallback={
                <div class="sidebar-empty">
                  {searchText() ? "No projects match your search" : "No projects yet"}
                </div>
              }
            >
              <For each={filteredProjects()}>
                {(project) => <ProjectCard project={project} />}
              </For>
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
