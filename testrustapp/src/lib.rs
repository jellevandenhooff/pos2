#[allow(warnings)]
mod bindings;

use bindings::Guest;

use bindings::component::testapp::custom_host::{Value, huh, query};

struct Component;

impl Guest for Component {
    /// Say hello!
    fn hello_world() -> String {
        println!("hello log");
        let result = query("SELECT * FROM test WHERE id = ?", &vec![Value::Int(1)]);
        return match result {
            None => format!("no result! {}", huh()).to_string(),
            Some(rows) => {
                for row in &rows {
                    println!("row!");
                    for col in row {
                        println!("col! {:?}", col);
                    }
                }

                let markup = maud::html! {
                    (maud::DOCTYPE)
                    html {
                        head {
                            title { "hello" }
                        }
                        body {
                            p { "another tiny change" }
                            p { "got " (rows.len()) " rows!" }
                            p { "also " (huh()) }
                            ul {
                                @for row in rows {
                                    li {
                                        @for col in row {
                                            (format!("{:?}", col)) " "
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                markup.into_string()

                // format!("got {} rows! {}", rows.len(), huh()).to_string()
            }
        };
        // format!("huh from component: {}", huh()).to_string()
    }
}

bindings::export!(Component with_types_in bindings);
