use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use ftbgui::book::QuestBook;
use ftbgui::editor;
use ftbgui::gui;

fn main() -> ExitCode {
    let started_at = Instant::now();
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next();

    if first.is_none() || first.as_deref().is_some_and(|value| value == "gui") {
        let path = arguments
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("gui_ftbquests"));
        return match gui::run(path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Ошибка GUI: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if first
        .as_deref()
        .is_some_and(|value| value == "new" || value == "create")
    {
        let path = arguments
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("new_ftbquests"));
        return match editor::run_interactive(&path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Ошибка редактора: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if first
        .as_deref()
        .is_some_and(|value| value == "help" || value == "--help" || value == "-h")
    {
        print_help();
        return ExitCode::SUCCESS;
    }

    let path = if first.as_deref().is_some_and(|value| value == "stats") {
        arguments
            .next()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ftbquests"))
    } else {
        first
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ftbquests"))
    };

    match QuestBook::load(&path) {
        Ok(book) => {
            let report = book.report();
            let technical_report = book.technical_report(started_at.elapsed());
            print!("{report}{technical_report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Ошибка: {error}");
            eprintln!("Используйте `ftbgui help` для справки.");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "ftbgui — парсер и простой редактор FTB Quests\n\n\
         Использование:\n\
         \x20 ftbgui                    Запустить графический редактор\n\
         \x20 ftbgui stats [путь]       Показать статистику книги\n\
         \x20 ftbgui [путь]             Показать статистику книги\n\
         \x20 ftbgui gui [папка]        Запустить графический редактор\n\
         \x20 ftbgui new [папка]        Создать новую книгу интерактивно\n\
         \x20 ftbgui help               Показать эту справку\n\n\
         Без аргументов открывается GUI. Новый проект по умолчанию сохраняется\n\
         в ./gui_ftbquests."
    );
}
