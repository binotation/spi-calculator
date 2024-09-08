//! Receive arithmetic expressions over SPI and compute them.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use heapless::{spsc::Queue, String};
use panic_semihosting as _; // logs messages to the host stderr; requires a debugger
use stm32u5::stm32u575::{interrupt, Interrupt, Peripherals, SPI1, USART2};

static mut USART2_PERIPHERAL: Option<USART2> = None;
static mut SPI1_PERIPHERAL: Option<SPI1> = None;
static mut BUFFER: Option<Queue<u16, 16>> = None;
static mut CALCULATOR: Option<CalculatorStateMachine> = None;

enum CalculatorState {
    Num1,
    Num2,
}

struct CalculatorStateMachine {
    num1: String<4>,
    num2: String<4>,
    op: char,
    state: CalculatorState,
}

impl Default for CalculatorStateMachine {
    fn default() -> Self {
        Self {
            num1: String::new(),
            num2: String::new(),
            op: '+',
            state: CalculatorState::Num1,
        }
    }
}

impl CalculatorStateMachine {
    /// Receive input and go to next state. Returns: if state transition was valid.
    fn transition(&mut self, input: char) -> bool {
        match self.state {
            CalculatorState::Num1 => {
                if self.num1.len() < self.num1.capacity() && input.is_ascii_digit() {
                    let _ = self.num1.push(input); // Always succeeds because of check
                    true
                } else if ['+', '-', '*', '/'].contains(&input) {
                    self.op = input;
                    self.state = CalculatorState::Num2;
                    true
                } else {
                    false
                }
            }
            CalculatorState::Num2 => {
                if self.num2.len() < self.num2.capacity() && input.is_ascii_digit() {
                    let _ = self.num2.push(input); // Always succeeds because of check
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Compute arithmetic expression.
    fn compute(&mut self) -> u32 {
        let result = match &self.state {
            CalculatorState::Num1 => self.num1.parse::<u32>().unwrap_or(0),

            CalculatorState::Num2 => match self.op {
                '+' => self
                    .num1
                    .parse::<u32>()
                    .unwrap_or(0)
                    .wrapping_add(self.num2.parse::<u32>().unwrap_or(0)),
                '*' => self
                    .num1
                    .parse::<u32>()
                    .unwrap_or(0)
                    .wrapping_mul(self.num2.parse::<u32>().unwrap_or(0)),
                '-' => self
                    .num1
                    .parse::<u32>()
                    .unwrap_or(0)
                    .saturating_sub(self.num2.parse::<u32>().unwrap_or(0)),
                '/' => self
                    .num1
                    .parse::<u32>()
                    .unwrap_or(0)
                    .wrapping_div(self.num2.parse::<u32>().unwrap_or(1)),
                _ => unreachable!(),
            },
        };
        // Reset state machine
        self.state = CalculatorState::Num1;
        self.num1.clear();
        self.num2.clear();
        result
    }
}

#[interrupt]
fn USART2() {
    // SAFETY: race condition where USART2_PERIPHERAL can be accessed before being set
    let usart2 = unsafe { USART2_PERIPHERAL.as_mut() }.unwrap();
    let buffer = unsafe { BUFFER.as_mut() }.unwrap();

    if usart2.isr_disabled().read().txfnf().bit_is_set() {
        match buffer.dequeue() {
            Some(byte) => {
                usart2.tdr().write(|w| unsafe { w.tdr().bits(byte) });
                if buffer.is_empty() {
                    usart2.cr1_disabled().modify(|_, w| w.txfnfie().clear_bit());
                }
            }
            None => {
                usart2.cr1_disabled().modify(|_, w| w.txfnfie().clear_bit());
            }
        }
    }
}

#[interrupt]
fn SPI1() {
    let spi1 = unsafe { SPI1_PERIPHERAL.as_mut() }.unwrap();
    let usart2 = unsafe { USART2_PERIPHERAL.as_mut() }.unwrap();
    let calculator = unsafe { CALCULATOR.as_mut() }.unwrap();
    let buffer = unsafe { BUFFER.as_mut() }.unwrap();

    if spi1.spi_sr().read().rxp().bit_is_set() {
        let received_byte = spi1.spi_rxdr().read().rxdr().bits() as u16;

        // Compute calculator state machine if '=' received
        if received_byte as u8 == b'=' {
            let chars = int_to_chars(calculator.compute());
            let mut non_zero_reached = false;
            let _ = buffer.enqueue(b'=' as u16);

            // Buffer chars
            for (i, &c) in chars.iter().enumerate() {
                if i == chars.len() - 1 && c == b'0' && !non_zero_reached {
                    // If last char is 0 and no non-zeroes, output 0
                    let _ = buffer.enqueue(c as u16);
                } else if c == b'0' && !non_zero_reached {
                    // Don't output leading zeroes
                    continue;
                } else if c > b'0' {
                    non_zero_reached = true;
                    let _ = buffer.enqueue(c as u16);
                } else {
                    // Output non-leading zeroes
                    let _ = buffer.enqueue(c as u16);
                }
            }
            // Output carriage return and line feed
            let _ = buffer.enqueue(13);
            let _ = buffer.enqueue(10);
        } else if calculator.transition(received_byte as u8 as char) {
            // Input into calculator state machine
            // Echo byte if valid transition
            let _ = buffer.enqueue(received_byte);
        };

        if !buffer.is_empty() {
            usart2.cr1_disabled().modify(|_, w| w.txfnfie().set_bit());
        }
    }
}

fn int_to_chars(mut int: u32) -> [u8; 8] {
    let mut chars: [u8; 8] = [0; 8];
    for (i, power) in [10_000_000, 1_000_000, 100_000, 10_000, 1_000, 100, 10, 1]
        .iter()
        .enumerate()
    {
        let digit = int / power;
        chars[i] = (digit + 48) as u8;
        int -= digit * power;
    }
    chars
}

#[entry]
fn main() -> ! {
    // Device defaults to 4MHz clock

    let dp = Peripherals::take().unwrap();

    // Enable peripheral clocks - GPIOA, USART2, SPI1
    dp.RCC.ahb2enr1().write(|w| w.gpioaen().enabled());
    dp.RCC.apb1enr1().write(|w| w.usart2en().enabled());
    dp.RCC.apb2enr().write(|w| w.spi1en().enabled());

    // USART2: A2 (TX), A3 (RX) as AF 7
    // SPI1: A4 (NSS), A5 (SCK), A6 (MISO), A7 (MOSI) as AF 5
    dp.GPIOA.moder().write(|w| {
        w.mode2()
            .alternate()
            .mode3()
            .alternate()
            .mode4()
            .alternate()
            .mode5()
            .alternate()
            .mode6()
            .alternate()
            .mode7()
            .alternate()
    });
    dp.GPIOA.ospeedr().write(|w| {
        w.ospeed2()
            .very_high_speed()
            .ospeed3()
            .very_high_speed()
            .ospeed4()
            .very_high_speed()
            .ospeed5()
            .very_high_speed()
            .ospeed6()
            .very_high_speed()
            .ospeed7()
            .very_high_speed()
    });
    dp.GPIOA.afrl().write(|w| {
        w.afsel2()
            .af7()
            .afsel3()
            .af7()
            .afsel4()
            .af5()
            .afsel5()
            .af5()
            .afsel6()
            .af5()
            .afsel7()
            .af5()
    });

    // USART2: Configure baud rate 9600
    dp.USART2.brr().write(|w| unsafe { w.bits(417) }); // 4Mhz / 9600 approx. 417

    // SPI1: Enable receive packet interrupt
    dp.SPI1.spi_ier().write(|w| w.rxpie().set_bit());
    dp.SPI1.spi_cr1().write(|w| w.spe().set_bit());

    // Enable USART, transmitter and RXNE interrupt
    dp.USART2
        .cr1_disabled()
        .write(|w| w.te().set_bit().ue().set_bit());

    unsafe {
        BUFFER = Some(Queue::default());
        CALCULATOR = Some(CalculatorStateMachine::default());
        // Unmask global interrupts
        cortex_m::peripheral::NVIC::unmask(Interrupt::SPI1);
        cortex_m::peripheral::NVIC::unmask(Interrupt::USART2);
        SPI1_PERIPHERAL = Some(dp.SPI1);
        USART2_PERIPHERAL = Some(dp.USART2);
    }

    #[allow(clippy::empty_loop)]
    loop {}
}
