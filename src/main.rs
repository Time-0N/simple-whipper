use iced::widget::{button, column, row};
use iced::{Element, Subscription, Task, Theme};
use iced_term::TerminalView;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Whipper GUI")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::Dark)
        .run()
}

#[derive(Debug, Clone)]
enum Msg {
    Calibrate,
    Rip,
    Term(iced_term::Event),
}

struct App {
    term: iced_term::Terminal,
    next_id: u64,
    running: bool,
}

fn make_term(id: u64, program: &str, args: &[&str]) -> iced_term::Terminal {
    let settings = iced_term::settings::Settings {
        backend: iced_term::settings::BackendSettings {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
        ..Default::default()
    };
    iced_term::Terminal::new(id, settings).expect("terminal")
}

impl App {
    fn new() -> (Self, Task<Msg>) {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
        (
            Self {
                term: make_term(0, &shell, &[]),
                next_id: 1,
                running: false,
            },
            Task::none(),
        )
    }

    fn start(&mut self, program: &str, args: &[&str]) {
        self.term = make_term(self.next_id, program, args);
        self.next_id += 1;
        self.running = true;
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Calibrate => self.start(
                "sh",
                &["-c", "whipper drive analyze && whipper offset find"],
            ),
            Msg::Rip => self.start("whipper", &["cd", "rip"]),
            Msg::Term(iced_term::Event::BackendCall(_, cmd)) => {
                if let iced_term::actions::Action::Shutdown =
                    self.term.handle(iced_term::Command::ProxyToBackend(cmd))
                {
                    self.running = false; // child exited
                }
            }
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Msg> {
        self.term.subscription().map(Msg::Term)
    }

    fn view(&self) -> Element<'_, Msg> {
        let mut calc = button("Calibrate drive");
        let mut rip = button("Rip CD");
        if !self.running {
            calc = calc.on_press(Msg::Calibrate);
            rip = rip.on_press(Msg::Rip);
        }
        column![
            row![calc, rip].spacing(6),
            TerminalView::show(&self.term).map(Msg::Term),
        ]
        .spacing(6)
        .into()
    }
}
