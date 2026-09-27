import { useEffect, useState } from "react";
import { describeError, getConfig, getStats, Stats } from "./api";
import HealthTab from "./tabs/HealthTab";
import GraphTab from "./tabs/GraphTab";
import SearchTab, { SearchSuggestion } from "./tabs/SearchTab";
import MemoriesTab from "./tabs/MemoriesTab";
import IntegrationsTab from "./tabs/IntegrationsTab";
import SettingsTab, { SettingsRequest } from "./tabs/SettingsTab";
import Onboarding from "./Onboarding";
import { sectionOf } from "./settings/model";

const TABS = ["Graph", "Search", "Memories", "Integrations", "Health", "Settings"] as const;
type Tab = (typeof TABS)[number];

const SETUP_DISMISSED = "br8n.setup.dismissed";

function setupDismissed(): boolean {
  try {
    return window.localStorage.getItem(SETUP_DISMISSED) === "1";
  } catch {
    return false;
  }
}

function dismissSetup() {
  try {
    window.localStorage.setItem(SETUP_DISMISSED, "1");
  } catch {}
}

export default function App() {
  const [tab, setTab] = useState<Tab>("Graph");
  // Tabs mount on first visit and then STAY mounted, hidden by CSS.
  // Unmounting threw away everything the tab had loaded, so returning to Graph
  // re-ran `/api/graph` — a store open and a full-graph scan — and re-simulated
  // the force layout from scratch, every single visit. It also discarded the
  // user's search results and their place in the graph. Still lazy: a tab you
  // never open never fetches anything.
  const [seen, setSeen] = useState<Set<Tab>>(() => new Set<Tab>(["Graph"]));
  const show = (t: Tab) => {
    setTab(t);
    setSeen((s) => (s.has(t) ? s : new Set(s).add(t)));
  };
  const [stats, setStats] = useState<Stats | null>(null);
  // Distinguishes "haven't loaded yet" from "tried and failed" — both leave
  // `stats` null, but only one of them should ever read as a header that will
  // never fill in. The header itself stays unobtrusive (no banner); Health
  // renders the message, from this same fetch rather than a second one.
  const [statsErr, setStatsErr] = useState<string | null>(null);
  const [memoryWrites, setMemoryWrites] = useState(0);
  const [indexRuns, setIndexRuns] = useState(0);
  const [settingsRequest, setSettingsRequest] = useState<SettingsRequest | null>(null);
  const refreshStats = () =>
    getStats()
      .then((s) => {
        setStats(s);
        setStatsErr(null);
      })
      .catch((e) => setStatsErr(describeError(e)));
  const openSources = () => {
    setSettingsRequest({ section: "Sources", n: Date.now() });
    show("Settings");
  };
  const openSetting = (key: string) => {
    setSettingsRequest({ section: sectionOf(key), n: Date.now(), focus: key });
    show("Settings");
  };
  const [setup, setSetup] = useState<"deciding" | "open" | "closed">("deciding");
  const [suggestion, setSuggestion] = useState<SearchSuggestion | null>(null);
  useEffect(() => {
    if (setup !== "deciding") return;
    if (setupDismissed() || statsErr !== null) {
      setSetup("closed");
      return;
    }
    if (!stats) return;
    if (stats.documents > 0) {
      setSetup("closed");
      return;
    }
    getConfig()
      .then((c) => setSetup(c.effective.sources.length === 0 ? "open" : "closed"))
      .catch(() => setSetup("closed"));
  }, [stats, statsErr, setup]);
  const skipSetup = () => {
    dismissSetup();
    setSetup("closed");
  };
  const finishSetup = (query: string) => {
    dismissSetup();
    refreshStats();
    setIndexRuns((n) => n + 1);
    setSuggestion({ q: query, n: Date.now() });
    setSetup("closed");
    show("Search");
  };
  const [highlight, setHighlight] = useState<Set<string>>(new Set());
  const onHits = (ids: string[]) => setHighlight(new Set(ids));
  useEffect(() => {
    refreshStats();
  }, []);
  return (
    <div className="h-screen flex flex-col">
      <header className="h-14 bg-surface border-b border-line flex items-center gap-7 px-6 shrink-0">
        <div className="flex items-center gap-2.5">
          <span className="font-serif font-semibold text-[19px]">br8n</span>
          <span className="w-[7px] h-[7px] rounded-full bg-accent" />
        </div>
        <nav className="flex gap-1">
          {TABS.map((t) => (
            <button key={t} onClick={() => { setSetup((s) => (s === "open" ? "closed" : s)); show(t); }}
              className={`px-3.5 py-1.5 rounded-md text-sm ${t === tab && setup !== "open" ? "font-semibold text-accent bg-accent-soft" : "font-medium text-muted"}`}>
              {t}
            </button>
          ))}
        </nav>
        <div className="grow" />
        <span className="font-mono text-xs text-muted">
          {stats ? `${stats.documents} docs · ${stats.chunks.toLocaleString()} chunks` : statsErr !== null ? "stats unavailable" : "—"}
        </span>
      </header>
      <main className="grow p-4 min-h-0">
        {setup === "deciding" && <p className="text-sm text-muted p-4">Loading…</p>}
        {setup === "open" && <Onboarding onSkip={skipSetup} onDone={finishSetup} />}
        {setup === "closed" && <>
        {seen.has("Graph") && (
          <div className={tab === "Graph" ? "h-full" : "hidden"}>
            <GraphTab highlight={highlight} reloads={memoryWrites + indexRuns} onAddSource={openSources} onSetup={() => setSetup("open")} />
          </div>
        )}
        {seen.has("Search") && (
          <div className={tab === "Search" ? "h-full" : "hidden"}>
            <SearchTab onHits={onHits} suggestion={suggestion} />
          </div>
        )}
        {seen.has("Memories") && (
          <div className={tab === "Memories" ? "h-full" : "hidden"}>
            <MemoriesTab onWritten={() => setMemoryWrites((n) => n + 1)} />
          </div>
        )}
        {seen.has("Integrations") && (
          <div className={tab === "Integrations" ? "h-full" : "hidden"}>
            <IntegrationsTab stats={stats} active={tab === "Integrations"} onOpenSources={openSources} />
          </div>
        )}
        {seen.has("Health") && (
          <div className={tab === "Health" ? "h-full" : "hidden"}>
            {/* `active` stops the 2s progress poll while the tab is hidden —
                staying mounted must not mean polling forever in the background. */}
            <HealthTab stats={stats} err={statsErr} active={tab === "Health"} onOpenSetting={openSetting} />
          </div>
        )}
        {seen.has("Settings") && (
          <div className={tab === "Settings" ? "h-full" : "hidden"}>
            <SettingsTab
              stats={stats}
              request={settingsRequest}
              onSaved={refreshStats}
              onIndexed={() => {
                refreshStats();
                setIndexRuns((n) => n + 1);
              }}
            />
          </div>
        )}
        </>}
      </main>
    </div>
  );
}
