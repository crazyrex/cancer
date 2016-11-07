// Copyleft (ↄ) meh. <meh@schizofreni.co> | http://meh.schizofreni.co
//
// This file is part of cancer.
//
// cancer is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// cancer is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with cancer.  If not, see <http://www.gnu.org/licenses/>.

use std::rc::Rc;
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::collections::HashMap;
use std::vec;
use std::hash::BuildHasherDefault;
use fnv::FnvHasher;
use unicode_segmentation::UnicodeSegmentation;

use picto::Region;
use error;
use config;
use platform::{Key, key};
use style::{self, Style};
use terminal::{Access, Action, Terminal, Cursor, Iter};
use terminal::touched::{self, Touched};
use terminal::cell::{self, Cell};
use terminal::cursor;
use terminal::grid;

macro_rules! overlay {
	($term:ident; times $block:block) => ({
		for _ in 0 .. $term.times.unwrap_or(1) {
			$block
		}
	});

	($term:ident; cursor) => ({
		let x = $term.cursor.x();
		let y = $term.cursor.y();

		if let Cell::Reference(offset) = *overlay!($term; cell (x, y)) {
			(x - offset as u32, y)
		}
		else {
			(x, y)
		}
	});

	($term:ident; cursor absolute) => ({
		let     (x, y) = overlay!($term; cursor);
		let mut offset = ($term.inner.region.height as u32 - 1 - y) + $term.scroll;

		if $term.status.is_some() {
			offset -= 1;
		}

		(x, offset)
	});

	($term:ident; cursor $($travel:tt)*) => ({
		let before = overlay!($term; cursor absolute);
		$term.touched.push($term.cursor.position());

		let r = $term.cursor.travel(cursor::$($travel)*);

		let after = overlay!($term; cursor absolute);
		$term.touched.push($term.cursor.position());

		if let Some(select) = $term.select.as_mut() {
			Overlay::select(&mut $term.touched, select, &mut $term.changed, before, after);
		}

		r
	});

	($term:ident; move end) => ({
		overlay!($term; cursor Position(Some($term.inner.columns() - 1), None));
		overlay!($term; status position!);
	});

	($term:ident; move start) => ({
		overlay!($term; cursor Position(Some(0), None));
		overlay!($term; status position!);
	});

	($term:ident; move left) => ({
		if overlay!($term; cursor Left(1)).is_some() {
			if let Some(&Selection::Normal { .. }) = $term.select.as_ref() {
				if overlay!($term; cursor Up(1)).is_some() {
					overlay!($term; scroll up);
				}

				overlay!($term; cursor Position(Some($term.inner.region.width - 1), None));
			}
		}

		overlay!($term; status position!);
	});

	($term:ident; move right) => ({
		if overlay!($term; cursor Right(1)).is_some() {
			if let Some(&Selection::Normal { .. }) = $term.select.as_ref() {
				if overlay!($term; cursor Down(1)).is_some() {
					overlay!($term; scroll down);
				}

				overlay!($term; cursor Position(Some(0), None));
			}
		}

		overlay!($term; status position!);
	});

	($term:ident; move down) => ({
		if overlay!($term; cursor Down(1)).is_some() {
			overlay!($term; scroll down);
		}

		overlay!($term; status position!);
	});

	($term:ident; move up) => ({
		if overlay!($term; cursor Up(1)).is_some() {
			overlay!($term; scroll up);
		}

		overlay!($term; status position!);
	});

	($term:ident; status mode $name:expr) => ({
		if let Some(status) = $term.status.as_mut() {
			overlay!($term; touched line ($term.inner.region.height) - 1);
			status.mode($name);
		}
	});

	($term:ident; status position!) => ({
		if $term.status.is_some() {
			let (x, y) = overlay!($term; cursor absolute);
			let x      = x + 1;
			let y      = $term.inner.grid.back().len() as u32 + $term.inner.grid.view().len() as u32 - y;
			overlay!($term; status position (x, y));
		}
	});

	($term:ident; status position $pair:expr) => ({
		if let Some(status) = $term.status.as_mut() {
			overlay!($term; touched line ($term.inner.region.height) - 1);
			status.position($pair);
		}
	});

	($term:ident; scroll up) => ({
		if $term.scroll < $term.inner.grid.back().len() as u32 + if $term.status.is_some() { 1 } else { 0 } {
			$term.touched.all();
			$term.scroll += 1;

			overlay!($term; status position!);
		}
	});

	($term:ident; scroll down) => ({
		if $term.scroll > 0 {
			$term.touched.all();
			$term.scroll -= 1;

			overlay!($term; status position!);
		}
	});

	($term:ident; touched all) => (
		$term.touched.all();
	);

	($term:ident; touched line $y:expr) => (
		$term.touched.line($y);
	);

	($term:ident; touched ($x:expr, $y:expr)) => (
		$term.touched.mark($x, $y);
	);

	($term:ident; touched $pair:expr) => (
		$term.touched.push($pair);
	);

	($term:ident; cell ($x:expr, $y:expr)) => ({
		$term.get($x, $y)
	});

	($term:ident; mut cell ($x:expr, $y:expr)) => ({
		let view   = $term.inner.grid.view();
		let offset = (view.len() as u32 - 1 - y) + $term.scroll;

		if !$term.changed.contains_key(&(x, offset)) {
			let cell = $term.get(x, y).clone();
			$term.changed.insert((x, offset), cell);
		}

		$term.changed.get_mut(&(x, offset)).unwrap()
	});
}

pub type Changed = HashMap<(u32, u32), Cell, BuildHasherDefault<FnvHasher>>;

#[derive(Debug)]
pub struct Overlay {
	inner:   Terminal,
	cache:   Vec<u8>,
	touched: Touched,

	scroll:  u32,
	cursor:  Cursor,
	changed: Changed,
	status:  Option<Status>,

	prefix: Option<u8>,
	times:  Option<u32>,
	select: Option<Selection>,
}

#[derive(Debug)]
pub enum Selection {
	Normal {
		start: (u32, u32),
		end:   (u32, u32)
	},

	Block(Region),
}

impl Overlay {
	pub fn new(inner: Terminal) -> Self {
		let mut cursor = inner.cursor.clone();
		{
			let tmp = cursor.foreground;
			cursor.foreground = cursor.background;
			cursor.background = tmp;
		}

		let status = inner.config.style().status().map(|c| {
			cursor.travel(cursor::Up(1));
			cursor.scroll = (0, inner.rows() - 2);

			let mut status = Status::new(c, inner.columns());
			status.mode("NORMAL");

			let (x, y) = cursor.position();
			let y      = inner.grid.back().len() as u32 + y + 2;
			status.position((x, y));

			status
		});;

		Overlay {
			inner:   inner,
			touched: Touched::default(),
			cache:   Vec::new(),

			scroll:  0,
			cursor:  cursor,
			changed: Default::default(),
			status:  status,

			prefix: None,
			times:  None,
			select: None,
		}
	}

	pub fn into_inner<W: Write>(mut self, output: W) -> error::Result<Terminal> {
		try!(self.inner.handle(self.cache, output));
		Ok(self.inner)
	}

	pub fn get(&self, x: u32, y: u32) -> &Cell {
		if let Some(status) = self.status.as_ref() {
			if y == self.inner.rows() - 1 {
				return &status[x as usize];
			}
		}

		let     back   = self.inner.grid.back();
		let     view   = self.inner.grid.view();
		let mut offset = (view.len() as u32 - 1 - y) + self.scroll;

		if self.status.is_some() {
			offset -= 1;
		}

		if let Some(cell) = self.changed.get(&(x, offset)) {
			return cell;
		}

		if offset as usize >= view.len() {
			&back[back.len() - 1 - (offset as usize - view.len())][x as usize]
		}
		else {
			&view[view.len() - 1 - offset as usize][x as usize]
		}
	}

	pub fn row(&self, y: u32) -> &grid::Row {
		let back = self.inner.grid.back();
		let view = self.inner.grid.view();

		if y as usize >= view.len() {
			&back[back.len() - 1 - (y as usize - view.len())]
		}
		else {
			&view[view.len() - 1 - y as usize]
		}
	}

	pub fn cursor(&self) -> cursor::Cell {
		let (x, y) = overlay!(self; cursor);
		cursor::Cell::new(&self.cursor, cell::Position::new(x, y, self.get(x, y)))
	}

	/// Get an iterator over positioned cells.
	pub fn iter<'a, T: Iterator<Item = (u32, u32)>>(&'a self, iter: T) -> Iter<Self, T> {
		Iter::new(self, iter)
	}

	pub fn key(&mut self, key: Key) -> (vec::IntoIter<Action>, touched::Iter) {
		use platform::key::{Value, Button, Keypad};

		let mut actions = Vec::new();
		let     new     = self.prefix.is_none();

		// Check if the key is a number that makes operations run N times, if so
		// bail out early.
		if let Value::Char(ref ch) = *key.value() {
			if let Ok(number) = ch.parse::<u32>() {
				if self.times.is_some() || number != 0 {
					if let Some(times) = self.times.take() {
						self.times = Some(times * 10 + number);
					}
					else {
						self.times = Some(number);
					}

					return (actions.into_iter(), self.touched.iter(self.inner.region));
				}
			}
		}

		match *key.value() {
			Value::Char(ref ch) => match &**ch {
				// Scroll up.
				"\x19" | "e" if key.modifier() == key::CTRL => overlay!(self; times {
					overlay!(self; scroll up);
				}),

				// Scroll down.
				"\x05" | "e" if key.modifier() == key::CTRL => overlay!(self; times {
					overlay!(self; scroll down);
				}),

				"$" => {
					overlay!(self; move end);
				}

				"^" | "0" => {
					overlay!(self; move start);
				}

				// Move cursor left, wrapping and scrolling.
				"h" if key.modifier().is_empty() => overlay!(self; times {
					overlay!(self; move left);
				}),

				// Move cursor down, scrolling.
				"j" if key.modifier().is_empty() => overlay!(self; times {
					overlay!(self; move down);
				}),

				// Move cursor up, scrolling.
				"k" if key.modifier().is_empty() => overlay!(self; times {
					overlay!(self; move up);
				}),

				// Move cursor right, wrapping and scrolling.
				"l" if key.modifier().is_empty() => overlay!(self; times {
					overlay!(self; move right);
				}),

				// Scroll to the top.
				"g" if key.modifier().is_empty() && self.prefix.is_none() => {
					self.prefix = Some(b'g');
				}

				"g" if key.modifier().is_empty() && self.prefix == Some(b'g') => {
					self.scroll = self.inner.grid.back().len() as u32;
					overlay!(self; status position!);
					overlay!(self; touched all);
				}

				// Scroll to the end.
				"G" if key.modifier() == key::SHIFT => {
					if let Some(times) = self.times {
						self.scroll = (self.inner.grid.back().len() as u32).saturating_sub(
							times - 1);

						if self.status.is_some() {
							self.scroll += 1;
						}
					}
					else {
						self.scroll = 0;
					}

					overlay!(self; status position!);
					overlay!(self; touched all);
				}

				// Region selection.
				"v" if key.modifier().is_empty() => {
					match self.select.take() {
						Some(Selection::Normal { .. }) => {
							overlay!(self; status mode "NORMAL");
						}

						Some(Selection::Block(region)) => {
							unimplemented!();
						}

						None => {
							overlay!(self; status mode "VISUAL");

							let (x, y) = overlay!(self; cursor);
							self.select = Some(Selection::Normal { start: (x, y), end: (x, y) });
						}
					}
				}

				// Block selection.
				"\x16" | "v" if key.modifier() == key::CTRL => {
					match self.select.take() {
						Some(Selection::Block(..)) => {
							overlay!(self; status mode "NORMAL");
						}

						Some(Selection::Normal { start, end }) => {
							unimplemented!();
						}

						None => {
							overlay!(self; status mode "VISUAL BLOCK");

							let (x, y) = overlay!(self; cursor absolute);
							self.select = Some(Selection::Block(Region::from(x, y, 1, 1)));
						}
					}
				}

				_ => {
					debug!(target: "cancer::terminal::overlay::unhandled", "key {:?}", key);
				}
			},

			Value::Button(ref button) => match button {
				&Button::Left => overlay!(self; times {
					overlay!(self; move left);
				}),

				&Button::Down => overlay!(self; times {
					overlay!(self; move down);
				}),

				&Button::Up => overlay!(self; times {
					overlay!(self; move up);
				}),

				&Button::Right => overlay!(self; times {
					overlay!(self; move right);
				}),

				_ => {
					debug!(target: "cancer::terminal::overlay::unhandled", "key {:?}", key);
				}
			},

			Value::Keypad(ref button) => match button {
				&Keypad::Left => overlay!(self; times {
					overlay!(self; move left);
				}),

				&Keypad::Down => overlay!(self; times {
					overlay!(self; move down);
				}),

				&Keypad::Up => overlay!(self; times {
					overlay!(self; move up);
				}),

				&Keypad::Right => overlay!(self; times {
					overlay!(self; move right);
				}),

				_ => {
					debug!(target: "cancer::terminal::overlay::unhandled", "key {:?}", key);
				}
			},
		}

		// Only remove the prefix if it hadn't just been set.
		if self.prefix.is_some() && !new {
			self.prefix = None;
		}

		// Send the current selection to be set on the clipboard.
		match self.select {
			Some(Selection::Normal { start, end }) => {
				for y in end.1 .. start.1 {
					println!("{:?}", y);
				}
			}

			Some(Selection::Block(region)) => {

			}

			None => ()
		}

		self.times = None;
		(actions.into_iter(), self.touched.iter(self.inner.region()))
	}

	pub fn handle<I: AsRef<[u8]>>(&mut self, input: I) {
		self.cache.extend(input.as_ref());
	}

	fn select(touched: &mut Touched, selection: &mut Selection, changed: &mut Changed, before: (u32, u32), after: (u32, u32)) {
		println!("{:?} {:?}", before, after);

		match *selection {
			Selection::Normal { ref mut start, ref mut end } => {
				println!("NORMAL: {:?} .. {:?}", start, end);
			}

			Selection::Block(ref mut region) => {
				println!("BLOCK: {:?}", region);
			}
		}
	}
}

impl Access for Overlay {
	fn access(&self, x: u32, y: u32) -> &Cell {
		self.get(x, y)
	}
}

impl Deref for Overlay {
	type Target = Terminal;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl DerefMut for Overlay {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

#[derive(Debug)]
pub struct Status {
	cols:  u32,
	style: Rc<Style>,

	inner:    Vec<Cell>,
	mode:     String,
	position: String,
}

impl Status {
	pub fn new(config: &config::style::Status, cols: u32) -> Self {
		let style = Rc::new(Style {
			foreground: Some(*config.foreground()),
			background: Some(*config.background()),
			attributes: config.attributes(),
		});

		Status {
			cols:  cols,
			style: style.clone(),

			inner:    vec![Cell::empty(style.clone()); cols as usize],
			mode:     "".into(),
			position: "".into(),
		}
	}

	pub fn mode<T: Into<String>>(&mut self, string: T) {
		let string = string.into();

		for (_, cell) in self.mode.graphemes(true).zip(self.inner.iter_mut()) {
			cell.make_empty(self.style.clone());
		}

		for (ch, cell) in string.graphemes(true).zip(self.inner.iter_mut()) {
			cell.make_occupied(ch, self.style.clone());
		}

		self.mode = string;
	}

	pub fn position(&mut self, (x, y): (u32, u32)) {
		let format = format!("{}:{}", y, x);

		for (_, cell) in self.position.graphemes(true).rev().zip(self.inner.iter_mut().rev()) {
			cell.make_empty(self.style.clone());
		}

		for (ch, cell) in format.graphemes(true).rev().zip(self.inner.iter_mut().rev()) {
			cell.make_occupied(ch, self.style.clone());
		}

		self.position = format;
	}
}

impl Deref for Status {
	type Target = Vec<Cell>;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}