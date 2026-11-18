use crate::config::{shorten_home, AppConfig};
use crate::discover::{DiscoverClient, TrendingPeriod, TrendingSkill};
use crate::installer::{install_skill, sync_all_symlinks, uninstall_skill, update_skills};
use crate::models::{AgentTarget, SkillRecord, SkillSource, UpdateStatus};
use crate::scanner::scan_all_skills;
use crate::sources::UpdateChecker;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::{Frame, Terminal};
use std::collections::HashSet;
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    MainMenu,
    CheckResults,
    UpdateMenu,
    DiscoverMenu,
    DiscoverSearchInput,
    DiscoverList {
        title: String,
        period: Option<TrendingPeriod>,
    },
    ConfirmInstallTrending {
        skill: TrendingSkill,
        return_title: String,
        return_period: Option<TrendingPeriod>,
    },
    InstallInput,
    BrowseList {
        include_system: bool,
    },
    SkillDetail {
        skill_idx: usize,
        from_include_system: bool,
    },
    LinkPreview {
        preview_lines: Vec<String>,
    },
    UninstallMenu,
    ConfirmUninstall {
        name: String,
    },
    ActionResult {
        title: String,
        ok: bool,
        messages: Vec<String>,
    },
}

enum BgEvent {
    CheckDone {
        skills: Vec<SkillRecord>,
        goto: Option<Screen>,
    },
    DiscoverDone {
        title: String,
        period: Option<TrendingPeriod>,
        result: std::result::Result<Vec<TrendingSkill>, String>,
    },
    ActionDone {
        title: String,
        ok: bool,
        messages: Vec<String>,
        refresh_after: bool,
    },
}

struct MenuEntry {
    key: char,
    title: &'static str,
    subtitle: &'static str,
}

const MAIN_MENU: &[MenuEntry] = &[
    MenuEntry {
        key: '1',
        title: "Verificar atualizações no PC",
        subtitle: "Varre todas as skills e compara com GitHub, npx skills, clones locais e ai-memory",
    },
    MenuEntry {
        key: '2',
        title: "Atualizar skills desatualizadas",
        subtitle: "Atualiza todas de uma vez ou permite escolher qual atualizar",
    },
    MenuEntry {
        key: '3',
        title: "Descobrir skills mais baixadas (Dia / Semana / Mês)",
        subtitle: "Explora o ranking ao vivo do skills.sh e instala com 1 tecla",
    },
    MenuEntry {
        key: '4',
        title: "Instalar nova skill (GitHub / URL / Local)",
        subtitle: "Baixa de owner/repo, URL do GitHub ou pasta local para ~/.agents/skills + symlinks",
    },
    MenuEntry {
        key: '5',
        title: "Ver todas as skills instaladas",
        subtitle: "Explora cada skill, descrição, agentes vinculados e detalhes",
    },
    MenuEntry {
        key: '6',
        title: "Sincronizar symlinks entre agentes",
        subtitle: "Vincula skills de ~/.agents/skills para Claude, Antigravity e Codex",
    },
    MenuEntry {
        key: '7',
        title: "Remover uma skill instalada",
        subtitle: "Remove uma skill de ~/.agents/skills e limpa seus symlinks",
    },
    MenuEntry {
        key: '0',
        title: "Sair",
        subtitle: "Fecha o skillctl",
    },
];

const DISCOVER_MENU: &[MenuEntry] = &[
    MenuEntry {
        key: '1',
        title: "Mais baixadas do DIA (últimas 24h)",
        subtitle: "Top 30 skills com mais instalações nas últimas 24 horas",
    },
    MenuEntry {
        key: '2',
        title: "Mais baixadas da SEMANA (últimos 7 dias)",
        subtitle: "Top 30 skills mais instaladas na última semana + tendência",
    },
    MenuEntry {
        key: '3',
        title: "Mais baixadas do MÊS (últimos 30 dias)",
        subtitle: "Top 30 skills somando as últimas 4 semanas + tendência",
    },
    MenuEntry {
        key: '4',
        title: "Em alta hoje (maior crescimento vs ontem)",
        subtitle: "Skills que deram o maior salto de instalações hoje",
    },
    MenuEntry {
        key: '5',
        title: "Mais baixadas de TODOS OS TEMPOS",
        subtitle: "Ranking geral histórico do ecossistema skills.sh",
    },
    MenuEntry {
        key: '6',
        title: "Buscar por palavra-chave no catálogo",
        subtitle: "Pesquise por tema (ex.: react, pdf, design, docker, python)",
    },
    MenuEntry {
        key: '0',
        title: "Voltar ao menu principal",
        subtitle: "Retorna para a tela inicial",
    },
];

struct App {
    config: AppConfig,
    skills: Vec<SkillRecord>,
    checked_once: bool,
    screen: Screen,
    menu_state: ListState,
    discover_menu_state: ListState,
    table_state: TableState,
    discover_table_state: TableState,
    list_state: ListState,
    trending_items: Vec<TrendingSkill>,
    install_input: String,
    search_input: String,
    busy: bool,
    busy_label: String,
    spinner_tick: usize,
    tx: mpsc::UnboundedSender<BgEvent>,
    rx: mpsc::UnboundedReceiver<BgEvent>,
}

impl App {
    fn new(config: AppConfig, skills: Vec<SkillRecord>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        let mut discover_menu_state = ListState::default();
        discover_menu_state.select(Some(0));
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut discover_table_state = TableState::default();
        discover_table_state.select(Some(0));
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            config,
            skills,
            checked_once: false,
            screen: Screen::MainMenu,
            menu_state,
            discover_menu_state,
            table_state,
            discover_table_state,
            list_state,
            trending_items: Vec::new(),
            install_input: String::new(),
            search_input: String::new(),
            busy: false,
            busy_label: String::new(),
            spinner_tick: 0,
            tx,
            rx,
        }
    }

    fn user_skill_count(&self) -> usize {
        self.skills
            .iter()
            .filter(|s| !s.source.is_system_or_cloud())
            .count()
    }

    fn installed_names_lower(&self) -> HashSet<String> {
        self.skills
            .iter()
            .map(|s| s.name.to_lowercase())
            .collect()
    }

    fn outdated_indices(&self) -> Vec<usize> {
        self.skills
            .iter()
            .enumerate()
            .filter(|(_, s)| s.status.is_outdated() && s.source.can_update())
            .map(|(i, _)| i)
            .collect()
    }

    fn up_to_date_count(&self) -> usize {
        self.skills
            .iter()
            .filter(|s| s.status.is_up_to_date())
            .count()
    }

    fn browse_indices(&self, include_system: bool) -> Vec<usize> {
        self.skills
            .iter()
            .enumerate()
            .filter(|(_, s)| include_system || !s.source.is_system_or_cloud())
            .map(|(i, _)| i)
            .collect()
    }

    fn removable_skills(&self) -> Vec<String> {
        let unified = self.config.unified_skills_dir();
        let Ok(canon_unified) = std::fs::canonicalize(&unified) else {
            return Vec::new();
        };
        let mut names: Vec<String> = self
            .skills
            .iter()
            .filter(|s| s.canonical_path.parent() == Some(canon_unified.as_path()))
            .filter_map(|s| {
                s.canonical_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_string())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn start_check(&mut self, goto: Option<Screen>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = "Varrendo o computador e consultando atualizações...".to_string();

        let tx = self.tx.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let mut skills = scan_all_skills(&config).unwrap_or_default();
            let checker = UpdateChecker::new();
            checker.check_all(&mut skills).await;
            let _ = tx.send(BgEvent::CheckDone { skills, goto });
        });
    }

    fn start_discover_period(&mut self, period: TrendingPeriod) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = format!("Buscando ranking '{}' em skills.sh...", period.label());

        let tx = self.tx.clone();
        let title = period.label().to_string();
        tokio::spawn(async move {
            let client = DiscoverClient::new();
            let res = client
                .fetch_trending(period, 30)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(BgEvent::DiscoverDone {
                title,
                period: Some(period),
                result: res,
            });
        });
    }

    fn start_discover_search(&mut self, query: String) {
        if self.busy || query.trim().is_empty() {
            return;
        }
        self.busy = true;
        self.busy_label = format!("Pesquisando '{query}' em skills.sh...");

        let tx = self.tx.clone();
        let title = format!("Busca: \"{query}\"");
        tokio::spawn(async move {
            let client = DiscoverClient::new();
            let res = client
                .search_skills(&query, 30)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(BgEvent::DiscoverDone {
                title,
                period: None,
                result: res,
            });
        });
    }

    fn start_update(&mut self, targets: Vec<SkillRecord>, title: String) {
        if self.busy || targets.is_empty() {
            return;
        }
        self.busy = true;
        self.busy_label = format!("{title}...");

        let tx = self.tx.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || update_skills(&config, &targets, false))
                .await;
            match res {
                Ok(Ok(report)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: true,
                        messages: report.messages,
                        refresh_after: true,
                    });
                }
                Ok(Err(e)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ {e}")],
                        refresh_after: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ Erro interno: {e}")],
                        refresh_after: false,
                    });
                }
            }
        });
    }

    fn start_install(&mut self, spec: String) {
        if self.busy || spec.trim().is_empty() {
            return;
        }
        self.busy = true;
        self.busy_label = format!("Instalando '{spec}' em ~/.agents/skills...");

        let tx = self.tx.clone();
        let config = self.config.clone();
        let title = format!("Instalação: {spec}");
        tokio::spawn(async move {
            let targets = AgentTarget::default_symlink_targets();
            let res = tokio::task::spawn_blocking(move || {
                install_skill(&config, &spec, None, &targets, false)
            })
            .await;
            match res {
                Ok(Ok(report)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: true,
                        messages: report.messages,
                        refresh_after: true,
                    });
                }
                Ok(Err(e)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ {e}")],
                        refresh_after: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ Erro interno: {e}")],
                        refresh_after: false,
                    });
                }
            }
        });
    }

    fn start_sync_links(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = "Criando symlinks para Claude, Antigravity e Codex...".to_string();

        let tx = self.tx.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            let targets = AgentTarget::default_symlink_targets();
            let res =
                tokio::task::spawn_blocking(move || sync_all_symlinks(&config, &targets, false))
                    .await;
            match res {
                Ok(Ok(mut report)) => {
                    if report.messages.is_empty() {
                        report.push("✓ Todos os symlinks já estavam sincronizados.");
                    }
                    let _ = tx.send(BgEvent::ActionDone {
                        title: "Sincronização de symlinks".to_string(),
                        ok: true,
                        messages: report.messages,
                        refresh_after: true,
                    });
                }
                Ok(Err(e)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title: "Sincronização de symlinks".to_string(),
                        ok: false,
                        messages: vec![format!("✗ {e}")],
                        refresh_after: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title: "Sincronização de symlinks".to_string(),
                        ok: false,
                        messages: vec![format!("✗ Erro interno: {e}")],
                        refresh_after: false,
                    });
                }
            }
        });
    }

    fn start_uninstall(&mut self, name: String) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.busy_label = format!("Removendo '{name}'...");

        let tx = self.tx.clone();
        let config = self.config.clone();
        let title = format!("Remoção: {name}");
        tokio::spawn(async move {
            let res =
                tokio::task::spawn_blocking(move || uninstall_skill(&config, &name, false)).await;
            match res {
                Ok(Ok(report)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: true,
                        messages: report.messages,
                        refresh_after: true,
                    });
                }
                Ok(Err(e)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ {e}")],
                        refresh_after: false,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        title,
                        ok: false,
                        messages: vec![format!("✗ Erro interno: {e}")],
                        refresh_after: false,
                    });
                }
            }
        });
    }

    fn open_link_preview(&mut self) {
        let targets = AgentTarget::default_symlink_targets();
        let lines = match sync_all_symlinks(&self.config, &targets, true) {
            Ok(rep) if rep.messages.is_empty() => {
                vec!["✓ Todos os symlinks de ~/.agents/skills já estão sincronizados!".to_string()]
            }
            Ok(rep) => rep.messages,
            Err(e) => vec![format!("✗ {e}")],
        };
        self.screen = Screen::LinkPreview {
            preview_lines: lines,
        };
    }

    fn trigger_main_option(&mut self, idx: usize) -> bool {
        match idx {
            0 => {
                // [1] Verificar atualizações
                self.start_check(Some(Screen::CheckResults));
                false
            }
            1 => {
                // [2] Atualizar skills desatualizadas
                if self.checked_once {
                    self.list_state.select(Some(0));
                    self.screen = Screen::UpdateMenu;
                } else {
                    self.start_check(Some(Screen::UpdateMenu));
                }
                false
            }
            2 => {
                // [3] Descobrir skills mais baixadas
                self.discover_menu_state.select(Some(0));
                self.screen = Screen::DiscoverMenu;
                false
            }
            3 => {
                // [4] Instalar nova skill
                self.install_input.clear();
                self.screen = Screen::InstallInput;
                false
            }
            4 => {
                // [5] Ver todas as skills instaladas
                self.table_state.select(Some(0));
                self.screen = Screen::BrowseList {
                    include_system: false,
                };
                false
            }
            5 => {
                // [6] Sincronizar symlinks
                self.open_link_preview();
                false
            }
            6 => {
                // [7] Remover uma skill
                self.list_state.select(Some(0));
                self.screen = Screen::UninstallMenu;
                false
            }
            7 => {
                // [0] Sair
                true
            }
            _ => false,
        }
    }

    fn trigger_discover_option(&mut self, idx: usize) {
        match idx {
            0 => self.start_discover_period(TrendingPeriod::Day),
            1 => self.start_discover_period(TrendingPeriod::Week),
            2 => self.start_discover_period(TrendingPeriod::Month),
            3 => self.start_discover_period(TrendingPeriod::Hot),
            4 => self.start_discover_period(TrendingPeriod::AllTime),
            5 => {
                self.search_input.clear();
                self.screen = Screen::DiscoverSearchInput;
            }
            6 => {
                self.screen = Screen::MainMenu;
            }
            _ => {}
        }
    }
}

pub async fn run_tui(config: AppConfig) -> Result<()> {
    let initial_skills = scan_all_skills(&config)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config, initial_skills);
    app.start_check(None);

    let res = event_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        while let Ok(ev) = app.rx.try_recv() {
            match ev {
                BgEvent::CheckDone { skills, goto } => {
                    app.skills = skills;
                    app.checked_once = true;
                    app.busy = false;
                    app.busy_label.clear();
                    if let Some(target_screen) = goto {
                        app.list_state.select(Some(0));
                        app.table_state.select(Some(0));
                        app.screen = target_screen;
                    }
                }
                BgEvent::DiscoverDone {
                    title,
                    period,
                    result,
                } => {
                    app.busy = false;
                    app.busy_label.clear();
                    match result {
                        Ok(items) => {
                            app.trending_items = items;
                            app.discover_table_state.select(if app.trending_items.is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                            app.screen = Screen::DiscoverList { title, period };
                        }
                        Err(e) => {
                            app.screen = Screen::ActionResult {
                                title: format!("Ranking — {title}"),
                                ok: false,
                                messages: vec![format!("✗ {e}")],
                            };
                        }
                    }
                }
                BgEvent::ActionDone {
                    title,
                    ok,
                    messages,
                    refresh_after,
                } => {
                    app.busy = false;
                    app.busy_label.clear();
                    app.screen = Screen::ActionResult {
                        title,
                        ok,
                        messages,
                    };
                    if refresh_after {
                        if let Ok(fresh) = scan_all_skills(&app.config) {
                            app.skills = fresh;
                            app.checked_once = false;
                        }
                    }
                }
            }
        }

        if app.busy {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
        }

        terminal.draw(|f| render_ui(f, app))?;

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    return Ok(());
                }

                if app.busy {
                    continue;
                }

                if handle_key(app, key.code) {
                    return Ok(());
                }
            }
        }
    }
}

fn handle_key(app: &mut App, code: KeyCode) -> bool {
    match app.screen.clone() {
        Screen::MainMenu => match code {
            KeyCode::Char('0') | KeyCode::Char('q') => true,
            KeyCode::Char('1') => app.trigger_main_option(0),
            KeyCode::Char('2') => app.trigger_main_option(1),
            KeyCode::Char('3') => app.trigger_main_option(2),
            KeyCode::Char('4') => app.trigger_main_option(3),
            KeyCode::Char('5') => app.trigger_main_option(4),
            KeyCode::Char('6') => app.trigger_main_option(5),
            KeyCode::Char('7') => app.trigger_main_option(6),
            KeyCode::Down => {
                let cur = app.menu_state.selected().unwrap_or(0);
                let next = (cur + 1).min(MAIN_MENU.len() - 1);
                app.menu_state.select(Some(next));
                false
            }
            KeyCode::Up => {
                let cur = app.menu_state.selected().unwrap_or(0);
                app.menu_state.select(Some(cur.saturating_sub(1)));
                false
            }
            KeyCode::Enter => {
                let sel = app.menu_state.selected().unwrap_or(0);
                app.trigger_main_option(sel)
            }
            _ => false,
        },

        Screen::DiscoverMenu => match code {
            KeyCode::Char('0') | KeyCode::Esc => {
                app.screen = Screen::MainMenu;
                false
            }
            KeyCode::Char('1') => {
                app.trigger_discover_option(0);
                false
            }
            KeyCode::Char('2') => {
                app.trigger_discover_option(1);
                false
            }
            KeyCode::Char('3') => {
                app.trigger_discover_option(2);
                false
            }
            KeyCode::Char('4') => {
                app.trigger_discover_option(3);
                false
            }
            KeyCode::Char('5') => {
                app.trigger_discover_option(4);
                false
            }
            KeyCode::Char('6') => {
                app.trigger_discover_option(5);
                false
            }
            KeyCode::Down => {
                let cur = app.discover_menu_state.selected().unwrap_or(0);
                app.discover_menu_state
                    .select(Some((cur + 1).min(DISCOVER_MENU.len() - 1)));
                false
            }
            KeyCode::Up => {
                let cur = app.discover_menu_state.selected().unwrap_or(0);
                app.discover_menu_state.select(Some(cur.saturating_sub(1)));
                false
            }
            KeyCode::Enter => {
                let sel = app.discover_menu_state.selected().unwrap_or(0);
                app.trigger_discover_option(sel);
                false
            }
            _ => false,
        },

        Screen::DiscoverSearchInput => match code {
            KeyCode::Esc => {
                app.screen = Screen::DiscoverMenu;
                false
            }
            KeyCode::Enter => {
                let q = app.search_input.trim().to_string();
                if q == "0" || q.is_empty() {
                    app.screen = Screen::DiscoverMenu;
                } else {
                    app.start_discover_search(q);
                }
                false
            }
            KeyCode::Backspace => {
                app.search_input.pop();
                false
            }
            KeyCode::Char(c) => {
                app.search_input.push(c);
                false
            }
            _ => false,
        },

        Screen::DiscoverList { title, period } => {
            let len = app.trending_items.len();
            match code {
                KeyCode::Char('0') | KeyCode::Esc => {
                    app.screen = Screen::DiscoverMenu;
                    false
                }
                KeyCode::Char('1') => {
                    app.start_discover_period(TrendingPeriod::Day);
                    false
                }
                KeyCode::Char('2') => {
                    app.start_discover_period(TrendingPeriod::Week);
                    false
                }
                KeyCode::Char('3') => {
                    app.start_discover_period(TrendingPeriod::Month);
                    false
                }
                KeyCode::Char('4') => {
                    app.start_discover_period(TrendingPeriod::Hot);
                    false
                }
                KeyCode::Char('5') => {
                    app.start_discover_period(TrendingPeriod::AllTime);
                    false
                }
                KeyCode::Char('6') => {
                    app.search_input.clear();
                    app.screen = Screen::DiscoverSearchInput;
                    false
                }
                KeyCode::Down if len > 0 => {
                    let cur = app.discover_table_state.selected().unwrap_or(0);
                    app.discover_table_state.select(Some((cur + 1).min(len - 1)));
                    false
                }
                KeyCode::Up if len > 0 => {
                    let cur = app.discover_table_state.selected().unwrap_or(0);
                    app.discover_table_state.select(Some(cur.saturating_sub(1)));
                    false
                }
                KeyCode::PageDown if len > 0 => {
                    let cur = app.discover_table_state.selected().unwrap_or(0);
                    app.discover_table_state
                        .select(Some((cur + 10).min(len - 1)));
                    false
                }
                KeyCode::PageUp if len > 0 => {
                    let cur = app.discover_table_state.selected().unwrap_or(0);
                    app.discover_table_state.select(Some(cur.saturating_sub(10)));
                    false
                }
                KeyCode::Enter if len > 0 => {
                    if let Some(sel) = app.discover_table_state.selected() {
                        if let Some(item) = app.trending_items.get(sel).cloned() {
                            app.screen = Screen::ConfirmInstallTrending {
                                skill: item,
                                return_title: title,
                                return_period: period,
                            };
                        }
                    }
                    false
                }
                _ => false,
            }
        }

        Screen::ConfirmInstallTrending {
            skill,
            return_title,
            return_period,
        } => match code {
            KeyCode::Char('1') | KeyCode::Enter => {
                app.start_install(skill.install_spec());
                false
            }
            KeyCode::Char('0') | KeyCode::Esc => {
                app.screen = Screen::DiscoverList {
                    title: return_title,
                    period: return_period,
                };
                false
            }
            _ => false,
        },

        Screen::CheckResults => {
            let user_indices = app.browse_indices(false);
            match code {
                KeyCode::Char('0') | KeyCode::Esc => {
                    app.screen = Screen::MainMenu;
                    false
                }
                KeyCode::Char('1') => {
                    let outdated: Vec<SkillRecord> = app
                        .outdated_indices()
                        .into_iter()
                        .map(|i| app.skills[i].clone())
                        .collect();
                    if outdated.is_empty() {
                        app.screen = Screen::ActionResult {
                            title: "Atualização".to_string(),
                            ok: true,
                            messages: vec![
                                "✓ Todas as skills verificáveis já estão em dia!".to_string(),
                            ],
                        };
                    } else {
                        let count = outdated.len();
                        app.start_update(
                            outdated,
                            format!("Atualizando {count} skill(s) desatualizada(s)"),
                        );
                    }
                    false
                }
                KeyCode::Char('2') => {
                    app.list_state.select(Some(0));
                    app.screen = Screen::UpdateMenu;
                    false
                }
                KeyCode::Char('3') => {
                    app.start_check(Some(Screen::CheckResults));
                    false
                }
                KeyCode::Down => {
                    if !user_indices.is_empty() {
                        let cur = app.table_state.selected().unwrap_or(0);
                        app.table_state
                            .select(Some((cur + 1).min(user_indices.len() - 1)));
                    }
                    false
                }
                KeyCode::Up => {
                    let cur = app.table_state.selected().unwrap_or(0);
                    app.table_state.select(Some(cur.saturating_sub(1)));
                    false
                }
                KeyCode::Enter => {
                    if let Some(sel) = app.table_state.selected() {
                        if let Some(&real_idx) = user_indices.get(sel) {
                            app.screen = Screen::SkillDetail {
                                skill_idx: real_idx,
                                from_include_system: false,
                            };
                        }
                    }
                    false
                }
                _ => false,
            }
        }

        Screen::UpdateMenu => {
            let outdated_idxs = app.outdated_indices();
            let total_items = if outdated_idxs.is_empty() {
                0
            } else {
                outdated_idxs.len() + 1
            };

            match code {
                KeyCode::Char('0') | KeyCode::Esc => {
                    app.screen = Screen::MainMenu;
                    false
                }
                KeyCode::Char('1') if !outdated_idxs.is_empty() => {
                    let targets: Vec<SkillRecord> = outdated_idxs
                        .into_iter()
                        .map(|i| app.skills[i].clone())
                        .collect();
                    let n = targets.len();
                    app.start_update(targets, format!("Atualizando todas ({n} skills)"));
                    false
                }
                KeyCode::Char(c) if ('2'..='9').contains(&c) && !outdated_idxs.is_empty() => {
                    let item_pos = (c as usize) - ('2' as usize);
                    if let Some(&skill_idx) = outdated_idxs.get(item_pos) {
                        let s = app.skills[skill_idx].clone();
                        let name = s.name.clone();
                        app.start_update(vec![s], format!("Atualizando '{name}'"));
                    }
                    false
                }
                KeyCode::Down if total_items > 0 => {
                    let cur = app.list_state.selected().unwrap_or(0);
                    app.list_state.select(Some((cur + 1).min(total_items - 1)));
                    false
                }
                KeyCode::Up if total_items > 0 => {
                    let cur = app.list_state.selected().unwrap_or(0);
                    app.list_state.select(Some(cur.saturating_sub(1)));
                    false
                }
                KeyCode::Enter if total_items > 0 => {
                    let sel = app.list_state.selected().unwrap_or(0);
                    if sel == 0 {
                        let targets: Vec<SkillRecord> = outdated_idxs
                            .into_iter()
                            .map(|i| app.skills[i].clone())
                            .collect();
                        let n = targets.len();
                        app.start_update(targets, format!("Atualizando todas ({n} skills)"));
                    } else if let Some(&skill_idx) = outdated_idxs.get(sel - 1) {
                        let s = app.skills[skill_idx].clone();
                        let name = s.name.clone();
                        app.start_update(vec![s], format!("Atualizando '{name}'"));
                    }
                    false
                }
                _ => false,
            }
        }

        Screen::InstallInput => match code {
            KeyCode::Esc => {
                app.screen = Screen::MainMenu;
                false
            }
            KeyCode::Enter => {
                let spec = app.install_input.trim().to_string();
                if spec == "0" || spec.is_empty() {
                    app.screen = Screen::MainMenu;
                } else {
                    app.start_install(spec);
                }
                false
            }
            KeyCode::Backspace => {
                app.install_input.pop();
                false
            }
            KeyCode::Char(c) => {
                app.install_input.push(c);
                false
            }
            _ => false,
        },

        Screen::BrowseList { include_system } => {
            let indices = app.browse_indices(include_system);
            match code {
                KeyCode::Char('0') | KeyCode::Esc => {
                    app.screen = Screen::MainMenu;
                    false
                }
                KeyCode::Char('1') => {
                    app.table_state.select(Some(0));
                    app.screen = Screen::BrowseList {
                        include_system: !include_system,
                    };
                    false
                }
                KeyCode::Down if !indices.is_empty() => {
                    let cur = app.table_state.selected().unwrap_or(0);
                    app.table_state.select(Some((cur + 1).min(indices.len() - 1)));
                    false
                }
                KeyCode::Up if !indices.is_empty() => {
                    let cur = app.table_state.selected().unwrap_or(0);
                    app.table_state.select(Some(cur.saturating_sub(1)));
                    false
                }
                KeyCode::PageDown if !indices.is_empty() => {
                    let cur = app.table_state.selected().unwrap_or(0);
                    app.table_state
                        .select(Some((cur + 10).min(indices.len() - 1)));
                    false
                }
                KeyCode::PageUp if !indices.is_empty() => {
                    let cur = app.table_state.selected().unwrap_or(0);
                    app.table_state.select(Some(cur.saturating_sub(10)));
                    false
                }
                KeyCode::Enter if !indices.is_empty() => {
                    if let Some(sel) = app.table_state.selected() {
                        if let Some(&real_idx) = indices.get(sel) {
                            app.screen = Screen::SkillDetail {
                                skill_idx: real_idx,
                                from_include_system: include_system,
                            };
                        }
                    }
                    false
                }
                _ => false,
            }
        }

        Screen::SkillDetail {
            skill_idx,
            from_include_system,
        } => match code {
            KeyCode::Char('0') | KeyCode::Esc => {
                app.screen = Screen::BrowseList {
                    include_system: from_include_system,
                };
                false
            }
            KeyCode::Char('1') => {
                if let Some(s) = app.skills.get(skill_idx).cloned() {
                    if s.source.can_update() {
                        let name = s.name.clone();
                        app.start_update(vec![s], format!("Atualizando '{name}'"));
                    }
                }
                false
            }
            KeyCode::Char('2') => {
                if let Some(s) = app.skills.get(skill_idx) {
                    if let Some(dir_name) = s.canonical_path.file_name().and_then(|n| n.to_str()) {
                        let removable = app.removable_skills();
                        if removable.iter().any(|r| r == dir_name) {
                            app.screen = Screen::ConfirmUninstall {
                                name: dir_name.to_string(),
                            };
                        }
                    }
                }
                false
            }
            _ => false,
        },

        Screen::LinkPreview { .. } => match code {
            KeyCode::Char('0') | KeyCode::Esc => {
                app.screen = Screen::MainMenu;
                false
            }
            KeyCode::Char('1') | KeyCode::Enter => {
                app.start_sync_links();
                false
            }
            _ => false,
        },

        Screen::UninstallMenu => {
            let removable = app.removable_skills();
            match code {
                KeyCode::Char('0') | KeyCode::Esc => {
                    app.screen = Screen::MainMenu;
                    false
                }
                KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                    let idx = (c as usize) - ('1' as usize);
                    if let Some(name) = removable.get(idx) {
                        app.screen = Screen::ConfirmUninstall { name: name.clone() };
                    }
                    false
                }
                KeyCode::Down if !removable.is_empty() => {
                    let cur = app.list_state.selected().unwrap_or(0);
                    app.list_state
                        .select(Some((cur + 1).min(removable.len() - 1)));
                    false
                }
                KeyCode::Up if !removable.is_empty() => {
                    let cur = app.list_state.selected().unwrap_or(0);
                    app.list_state.select(Some(cur.saturating_sub(1)));
                    false
                }
                KeyCode::Enter if !removable.is_empty() => {
                    let sel = app.list_state.selected().unwrap_or(0);
                    if let Some(name) = removable.get(sel) {
                        app.screen = Screen::ConfirmUninstall { name: name.clone() };
                    }
                    false
                }
                _ => false,
            }
        }

        Screen::ConfirmUninstall { name } => match code {
            KeyCode::Char('1') => {
                app.start_uninstall(name);
                false
            }
            KeyCode::Char('0') | KeyCode::Esc => {
                app.screen = Screen::MainMenu;
                false
            }
            _ => false,
        },

        Screen::ActionResult { .. } => match code {
            KeyCode::Char('0') | KeyCode::Enter | KeyCode::Esc => {
                app.screen = Screen::MainMenu;
                false
            }
            KeyCode::Char('1') => {
                app.start_check(Some(Screen::CheckResults));
                false
            }
            _ => false,
        },
    }
}

fn render_ui(f: &mut Frame, app: &mut App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(f.area());

    render_header(f, app, layout[0]);

    if app.busy {
        render_busy(f, app, layout[1]);
        render_footer_text(
            f,
            " Aguarde... executando operação em andamento (Ctrl+C para interromper) ",
            layout[2],
        );
        return;
    }

    match app.screen.clone() {
        Screen::MainMenu => {
            render_main_menu(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite o número da opção (1-7) ou use ↑/↓ + Enter  |  [0] Sair ",
                layout[2],
            );
        }
        Screen::DiscoverMenu => {
            render_discover_menu(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite o número do ranking (1-6) ou use ↑/↓ + Enter  |  [0] Voltar ao menu principal ",
                layout[2],
            );
        }
        Screen::DiscoverSearchInput => {
            render_discover_search(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite o termo de busca e pressione [Enter]  |  [Esc] Voltar ",
                layout[2],
            );
        }
        Screen::DiscoverList { title, period } => {
            render_discover_list(f, app, &title, period, layout[1]);
            render_footer_text(
                f,
                " [Enter] Instalar selecionada   [1] Dia  [2] Semana  [3] Mês  [4] Alta  [5] Total  [6] Buscar   [0] Voltar ",
                layout[2],
            );
        }
        Screen::ConfirmInstallTrending { skill, .. } => {
            render_confirm_install_trending(f, &skill, layout[1]);
            render_footer_text(
                f,
                " [1] ou [Enter] Instalar agora em ~/.agents/skills + symlinks   |   [0] Voltar ao ranking ",
                layout[2],
            );
        }
        Screen::CheckResults => {
            render_check_results(f, app, layout[1]);
            let outdated_n = app.outdated_indices().len();
            let footer = if outdated_n > 0 {
                format!(
                    " [1] Atualizar TODAS ({outdated_n})   [2] Escolher qual atualizar   [3] Verificar novamente   [Enter] Detalhes   [0] Menu principal "
                )
            } else {
                " [3] Verificar novamente   [Enter] Ver detalhes da skill   [0] Voltar ao menu principal "
                    .to_string()
            };
            render_footer_text(f, &footer, layout[2]);
        }
        Screen::UpdateMenu => {
            render_update_menu(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite o número da opção (ou ↑/↓ + Enter)   |   [0] Voltar ao menu principal ",
                layout[2],
            );
        }
        Screen::InstallInput => {
            render_install_screen(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite a origem e pressione [Enter] para instalar   |   [Esc] Voltar ao menu principal ",
                layout[2],
            );
        }
        Screen::BrowseList { include_system } => {
            render_browse_list(f, app, include_system, layout[1]);
            let toggle_label = if include_system {
                "[1] Ocultar sistema/cloud"
            } else {
                "[1] Mostrar também sistema/cloud"
            };
            render_footer_text(
                f,
                &format!(
                    " [↑/↓] Navegar   [Enter] Abrir detalhes   {toggle_label}   [0] Voltar ao menu principal "
                ),
                layout[2],
            );
        }
        Screen::SkillDetail { skill_idx, .. } => {
            render_skill_detail(f, app, skill_idx, layout[1]);
            let mut opts = Vec::new();
            if let Some(s) = app.skills.get(skill_idx) {
                if s.source.can_update() {
                    opts.push("[1] Atualizar esta skill");
                }
                if let Some(dir_name) = s.canonical_path.file_name().and_then(|n| n.to_str()) {
                    if app.removable_skills().iter().any(|r| r == dir_name) {
                        opts.push("[2] Remover esta skill");
                    }
                }
            }
            opts.push("[0] Voltar para a lista");
            render_footer_text(f, &format!(" {} ", opts.join("   ")), layout[2]);
        }
        Screen::LinkPreview { preview_lines } => {
            render_link_preview(f, &preview_lines, layout[1]);
            render_footer_text(
                f,
                " [1] Aplicar sincronização agora   |   [0] Voltar ao menu principal ",
                layout[2],
            );
        }
        Screen::UninstallMenu => {
            render_uninstall_menu(f, app, layout[1]);
            render_footer_text(
                f,
                " Digite o número da skill (ou ↑/↓ + Enter)   |   [0] Voltar ao menu principal ",
                layout[2],
            );
        }
        Screen::ConfirmUninstall { name } => {
            render_confirm_uninstall(f, &name, layout[1]);
            render_footer_text(
                f,
                " [1] Sim, remover definitivamente   |   [0] Cancelar e voltar ",
                layout[2],
            );
        }
        Screen::ActionResult {
            title,
            ok,
            messages,
        } => {
            render_action_result(f, &title, ok, &messages, layout[1]);
            render_footer_text(
                f,
                " [1] Verificar atualizações novamente   |   [0] ou [Enter] Voltar ao menu principal ",
                layout[2],
            );
        }
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let user_cnt = app.user_skill_count();
    let outdated_cnt = app.outdated_indices().len();
    let ok_cnt = app.up_to_date_count();

    let mut spans = vec![
        Span::styled(
            " skillctl v0.2 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("Skills: {user_cnt}"),
            Style::default().fg(Color::White),
        ),
        Span::raw("  |  "),
    ];

    if app.busy {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let ch = frames[app.spinner_tick % frames.len()];
        spans.push(Span::styled(
            format!("{ch} Carregando..."),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.checked_once {
        spans.push(Span::styled(
            format!("↑ Para atualizar: {outdated_cnt}"),
            Style::default()
                .fg(if outdated_cnt > 0 {
                    Color::Yellow
                } else {
                    Color::Green
                })
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  |  "));
        spans.push(Span::styled(
            format!("✓ Em dia: {ok_cnt}"),
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::styled(
            "Use [1] para verificar atualizações",
            Style::default().fg(Color::Gray),
        ));
    }

    let p = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Gerenciador de Agent Skills "),
    );
    f.render_widget(p, area);
}

fn render_busy(f: &mut Frame, app: &App, area: Rect) {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let ch = frames[app.spinner_tick % frames.len()];
    let lines = vec![
        Line::raw(""),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!("  {ch}  "),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                app.busy_label.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::styled(
            "     Aguarde alguns segundos...",
            Style::default().fg(Color::Gray),
        ),
    ];

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Em andamento "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_main_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let outdated_cnt = app.outdated_indices().len();

    let items: Vec<ListItem> = MAIN_MENU
        .iter()
        .map(|entry| {
            let mut title_spans = vec![
                Span::styled(
                    format!(" [{}] ", entry.key),
                    Style::default()
                        .fg(Color::Black)
                        .bg(if entry.key == '0' {
                            Color::DarkGray
                        } else if entry.key == '3' {
                            Color::Magenta
                        } else {
                            Color::Cyan
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    entry.title,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ];

            if entry.key == '2' && app.checked_once && outdated_cnt > 0 {
                title_spans.push(Span::raw("  "));
                title_spans.push(Span::styled(
                    format!("({outdated_cnt} disponível/is)"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            ListItem::new(vec![
                Line::from(title_spans),
                Line::styled(
                    format!("       {}", entry.subtitle),
                    Style::default().fg(Color::Gray),
                ),
                Line::raw(""),
            ])
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Menu Principal — Escolha uma opção "),
        )
        .highlight_style(Style::default().bg(Color::Rgb(32, 44, 62)))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.menu_state);
}

fn render_discover_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = DISCOVER_MENU
        .iter()
        .map(|entry| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!(" [{}] ", entry.key),
                        Style::default()
                            .fg(Color::Black)
                            .bg(if entry.key == '0' {
                                Color::DarkGray
                            } else {
                                Color::Magenta
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        entry.title,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::styled(
                    format!("       {}", entry.subtitle),
                    Style::default().fg(Color::Gray),
                ),
                Line::raw(""),
            ])
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(" Menu Principal > Descobrir Skills Mais Baixadas (skills.sh) "),
        )
        .highlight_style(Style::default().bg(Color::Rgb(48, 32, 62)))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, &mut app.discover_menu_state);
}

fn render_discover_search(f: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::styled(
            "  Digite o que você procura no catálogo skills.sh:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Busca: ", Style::default().fg(Color::Magenta)),
            Span::styled(
                format!("{}_", app.search_input),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::styled(
            "  Exemplos: react, nextjs, design, pdf, playwright, docker, rust, security, seo",
            Style::default().fg(Color::Gray),
        ),
    ];

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta))
                .title(" Descobrir > Buscar no Catálogo skills.sh "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_discover_list(
    f: &mut Frame,
    app: &mut App,
    title: &str,
    period: Option<TrendingPeriod>,
    area: Rect,
) {
    let installed = app.installed_names_lower();
    let header = Row::new(vec![
        Cell::from("#"),
        Cell::from("Skill"),
        Cell::from("Downloads"),
        Cell::from("Tend. 8s"),
        Cell::from("Repositório"),
        Cell::from("Status"),
    ])
    .style(
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .trending_items
        .iter()
        .map(|it| {
            let is_inst = installed.contains(&it.name.to_lowercase())
                || installed.contains(&it.skill_id.to_lowercase());
            let (status_str, status_color) = if is_inst {
                ("✓ instalada", Color::Green)
            } else if it.is_official {
                ("★ oficial", Color::Cyan)
            } else {
                ("disponível", Color::DarkGray)
            };

            let spark = it.trend_sparkline.as_deref().unwrap_or("—");

            Row::new(vec![
                Cell::from(format!("#{}", it.rank)).style(Style::default().fg(Color::DarkGray)),
                Cell::from(it.name.clone()).style(
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(it.metric_label.clone()).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(spark.to_string()).style(Style::default().fg(Color::Cyan)),
                Cell::from(it.source.clone()).style(Style::default().fg(Color::Gray)),
                Cell::from(status_str).style(Style::default().fg(status_color)),
            ])
        })
        .collect();

    let period_hint = match period {
        Some(TrendingPeriod::Day) => " [1:DIA]  2:Sem  3:Mês  4:Alta  5:Total ",
        Some(TrendingPeriod::Week) => " 1:Dia  [2:SEM]  3:Mês  4:Alta  5:Total ",
        Some(TrendingPeriod::Month) => " 1:Dia  2:Sem  [3:MÊS]  4:Alta  5:Total ",
        Some(TrendingPeriod::Hot) => " 1:Dia  2:Sem  3:Mês  [4:ALTA]  5:Total ",
        Some(TrendingPeriod::AllTime) => " 1:Dia  2:Sem  3:Mês  4:Alta  [5:TOTAL] ",
        None => "",
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(28),
            Constraint::Length(18),
            Constraint::Length(10),
            Constraint::Min(22),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta))
            .title(format!(" Ranking skills.sh — {title} "))
            .title_bottom(Line::from(period_hint).right_aligned()),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(48, 32, 62))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.discover_table_state);
}

fn render_confirm_install_trending(f: &mut Frame, skill: &TrendingSkill, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Skill:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                skill.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Repositório:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(skill.source.clone(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Downloads:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                skill.metric_label.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Página web:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(skill.web_url(), Style::default().fg(Color::Blue)),
        ]),
        Line::from(vec![
            Span::styled("  Comando:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("skillctl install {}", skill.install_spec()),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::raw(""),
        Line::styled(
            "  Será instalada em ~/.agents/skills + symlinks automáticos (Claude, Antigravity, Codex).",
            Style::default().fg(Color::DarkGray),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  [1] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Instalar agora"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  [0] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Voltar ao ranking"),
        ]),
    ];

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .title(" Instalar Skill do Ranking "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_check_results(f: &mut Frame, app: &mut App, area: Rect) {
    let indices = app.browse_indices(false);

    let header = Row::new(vec![
        Cell::from("Status"),
        Cell::from("Skill"),
        Cell::from("Agentes"),
        Cell::from("Origem"),
        Cell::from("Resumo"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);

    let rows: Vec<Row> = indices
        .iter()
        .map(|&idx| {
            let s = &app.skills[idx];
            let (color, summary) = match &s.status {
                UpdateStatus::UpdateAvailable {
                    current,
                    latest,
                    summary,
                    ..
                } => (Color::Yellow, format!("{current} → {latest} ({summary})")),
                UpdateStatus::UpToDate { detail } => (Color::Green, detail.clone()),
                UpdateStatus::NotTrackable { reason } => (Color::DarkGray, reason.clone()),
                UpdateStatus::Error { message } => (Color::Red, message.clone()),
                UpdateStatus::Unchecked => (Color::Gray, "Não verificada".to_string()),
            };
            Row::new(vec![
                Cell::from(s.status.badge()).style(
                    Style::default()
                        .fg(color)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(s.name.clone()).style(Style::default().fg(Color::White)),
                Cell::from(s.agents_badge()).style(Style::default().fg(Color::Blue)),
                Cell::from(s.source.short_label()).style(Style::default().fg(Color::Gray)),
                Cell::from(summary).style(Style::default().fg(color)),
            ])
        })
        .collect();

    let outdated_cnt = app.outdated_indices().len();
    let title = format!(" Menu Principal > Verificar Atualizações ({outdated_cnt} para atualizar) ");

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(28),
            Constraint::Length(8),
            Constraint::Length(26),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(title),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(32, 44, 62))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_update_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let outdated_idxs = app.outdated_indices();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Menu Principal > Atualizar Skills ");

    if outdated_idxs.is_empty() {
        let p = Paragraph::new(vec![
            Line::raw(""),
            Line::styled(
                "  ✓ Todas as suas skills verificáveis já estão atualizadas!",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                "  Pressione [0] para voltar ao menu principal.",
                Style::default().fg(Color::Gray),
            ),
        ])
        .block(block);
        f.render_widget(p, area);
        return;
    }

    let mut items = Vec::new();
    items.push(ListItem::new(vec![
        Line::from(vec![
            Span::styled(
                " [1] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "Atualizar TODAS as {} skills desatualizadas de uma vez",
                    outdated_idxs.len()
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
    ]));

    for (pos, &skill_idx) in outdated_idxs.iter().enumerate() {
        let s = &app.skills[skill_idx];
        let num_label = if pos < 8 {
            format!(" [{}] ", pos + 2)
        } else {
            " [·] ".to_string()
        };
        let detail = match &s.status {
            UpdateStatus::UpdateAvailable {
                current,
                latest,
                summary,
                ..
            } => format!("{current} → {latest}  ({summary})"),
            _ => String::new(),
        };
        items.push(ListItem::new(vec![
            Line::from(vec![
                Span::styled(
                    num_label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    s.name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("   "),
                Span::styled(detail, Style::default().fg(Color::Yellow)),
            ]),
            Line::styled(
                format!("       Origem: {}", s.source.short_label()),
                Style::default().fg(Color::Gray),
            ),
        ]));
    }

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Rgb(32, 44, 62)))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_install_screen(f: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::styled(
            "  Digite o repositório do GitHub, URL ou caminho local da skill:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Origem: ", Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{}_", app.install_input),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::raw(""),
        Line::styled(
            "  Exemplos aceitos:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "    • owner/repo                  (ex.: heydamir/unslop)",
            Style::default().fg(Color::Gray),
        ),
        Line::styled(
            "    • owner/repo@nome-da-skill    (ex.: vercel-labs/skills@find-skills)",
            Style::default().fg(Color::Gray),
        ),
        Line::styled(
            "    • https://github.com/owner/repo",
            Style::default().fg(Color::Gray),
        ),
        Line::styled(
            "    • ~/caminho/para/pasta-local",
            Style::default().fg(Color::Gray),
        ),
        Line::raw(""),
        Line::styled(
            "  Destino: ~/.agents/skills/<nome> + symlinks automáticos para Claude, Antigravity e Codex.",
            Style::default().fg(Color::DarkGray),
        ),
    ];

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Menu Principal > Instalar Nova Skill "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_browse_list(f: &mut Frame, app: &mut App, include_system: bool, area: Rect) {
    let indices = app.browse_indices(include_system);
    let header = Row::new(vec![
        Cell::from("Status"),
        Cell::from("Skill"),
        Cell::from("Agentes"),
        Cell::from("Origem"),
        Cell::from("Caminho"),
    ])
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);

    let rows: Vec<Row> = indices
        .iter()
        .map(|&idx| {
            let s = &app.skills[idx];
            let status_color = match &s.status {
                UpdateStatus::UpdateAvailable { .. } => Color::Yellow,
                UpdateStatus::UpToDate { .. } => Color::Green,
                UpdateStatus::Error { .. } => Color::Red,
                _ => Color::DarkGray,
            };
            Row::new(vec![
                Cell::from(s.status.badge()).style(
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(s.name.clone()).style(Style::default().fg(Color::White)),
                Cell::from(s.agents_badge()).style(Style::default().fg(Color::Blue)),
                Cell::from(s.source.short_label()).style(Style::default().fg(Color::Gray)),
                Cell::from(shorten_home(&s.canonical_path, &app.config.home_dir))
                    .style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let scope = if include_system {
        "Todas (incluindo sistema e cloud)"
    } else {
        "Skills do usuário e projetos"
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(28),
            Constraint::Length(8),
            Constraint::Length(26),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(
                " Menu Principal > Skills Instaladas — {scope} ({}) ",
                indices.len()
            )),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(32, 44, 62))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_skill_detail(f: &mut Frame, app: &App, skill_idx: usize, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Detalhes da Skill ");

    let Some(s) = app.skills.get(skill_idx) else {
        f.render_widget(Paragraph::new("Skill não encontrada.").block(block), area);
        return;
    };

    let mut lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                format!("  {}  ", s.name),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{}]", s.status.badge()),
                Style::default().fg(match &s.status {
                    UpdateStatus::UpdateAvailable { .. } => Color::Yellow,
                    UpdateStatus::UpToDate { .. } => Color::Green,
                    UpdateStatus::Error { .. } => Color::Red,
                    _ => Color::Gray,
                }),
            ),
        ]),
        Line::raw(""),
    ];

    if !s.description.is_empty() {
        lines.push(Line::styled(
            format!("  {}", s.description),
            Style::default().fg(Color::White),
        ));
        lines.push(Line::raw(""));
    }

    lines.push(Line::from(vec![
        Span::styled("  Caminho: ", Style::default().fg(Color::DarkGray)),
        Span::raw(shorten_home(&s.canonical_path, &app.config.home_dir)),
    ]));

    let agents_str = if s.linked_agents.is_empty() {
        "Local / projeto".to_string()
    } else {
        s.linked_agents
            .iter()
            .map(|a| match a {
                AgentTarget::Agents => "Agents (~/.agents/skills)",
                AgentTarget::Claude => "Claude (~/.claude/skills)",
                AgentTarget::Antigravity => "Antigravity (~/.gemini/config/skills)",
                AgentTarget::Codex => "Codex (~/.codex/skills)",
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(Line::from(vec![
        Span::styled("  Agentes: ", Style::default().fg(Color::DarkGray)),
        Span::styled(agents_str, Style::default().fg(Color::Blue)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Origem:  ", Style::default().fg(Color::DarkGray)),
        Span::raw(s.source.short_label()),
    ]));

    if let SkillSource::CompanionGitClone { repo_dir, .. } = &s.source {
        lines.push(Line::from(vec![
            Span::styled("  Clone:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(shorten_home(repo_dir, &app.config.home_dir)),
        ]));
    }

    lines.push(Line::raw(""));
    match &s.status {
        UpdateStatus::UpdateAvailable {
            current,
            latest,
            summary,
            commits,
        } => {
            lines.push(Line::styled(
                format!("  ↑ Atualização disponível: {current} → {latest} ({summary})"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            for c in commits {
                lines.push(Line::styled(
                    format!("    • {c}"),
                    Style::default().fg(Color::Gray),
                ));
            }
        }
        UpdateStatus::UpToDate { detail } => {
            lines.push(Line::styled(
                format!("  ✓ {detail}"),
                Style::default().fg(Color::Green),
            ));
        }
        UpdateStatus::NotTrackable { reason } => {
            lines.push(Line::styled(
                format!("  • {reason}"),
                Style::default().fg(Color::Gray),
            ));
        }
        UpdateStatus::Error { message } => {
            lines.push(Line::styled(
                format!("  ! {message}"),
                Style::default().fg(Color::Red),
            ));
        }
        UpdateStatus::Unchecked => {
            lines.push(Line::styled(
                "  Ainda não verificada remotamente.",
                Style::default().fg(Color::Gray),
            ));
        }
    }

    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_link_preview(f: &mut Frame, preview_lines: &[String], area: Rect) {
    let mut lines = vec![
        Line::raw(""),
        Line::styled(
            "  Prévia dos symlinks que serão criados/atualizados a partir de ~/.agents/skills:",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];

    for l in preview_lines {
        let color = if l.starts_with('✓') {
            Color::Green
        } else if l.contains('⚠') {
            Color::Yellow
        } else {
            Color::Cyan
        };
        lines.push(Line::styled(format!("  {l}"), Style::default().fg(color)));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Menu Principal > Sincronizar Symlinks "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_uninstall_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let removable = app.removable_skills();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Menu Principal > Remover Skill (raiz ~/.agents/skills) ");

    if removable.is_empty() {
        let p = Paragraph::new("\n  Nenhuma skill removível encontrada em ~/.agents/skills.")
            .style(Style::default().fg(Color::Gray))
            .block(block);
        f.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = removable
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let badge = if i < 9 {
                format!(" [{}] ", i + 1)
            } else {
                " [·] ".to_string()
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    badge,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    name.clone(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Rgb(55, 30, 35)))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_confirm_uninstall(f: &mut Frame, name: &str, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::styled(
            format!("  Tem certeza que deseja remover a skill '{name}'?"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            format!("  Isso removerá ~/.agents/skills/{name} e os symlinks que apontam para ela."),
            Style::default().fg(Color::Gray),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  [1] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Sim, remover definitivamente"),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  [0] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  Cancelar e voltar"),
        ]),
    ];

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Confirmar Remoção "),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_action_result(f: &mut Frame, title: &str, ok: bool, messages: &[String], area: Rect) {
    let border_color = if ok { Color::Green } else { Color::Red };
    let mut lines = vec![Line::raw("")];
    for m in messages {
        let c = if m.starts_with('✓') {
            Color::Green
        } else if m.starts_with('✗') {
            Color::Red
        } else if m.contains('⚠') {
            Color::Yellow
        } else {
            Color::White
        };
        lines.push(Line::styled(format!("  {m}"), Style::default().fg(c)));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(format!(" Resultado — {title} ")),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_footer_text(f: &mut Frame, text: &str, area: Rect) {
    let p = Paragraph::new(Line::styled(
        text,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(p, area);
}
