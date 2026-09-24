# playmmix

[![playmmix stepping through "Is 13 prime?": registers change with each step, then the answer prints](docs/img/playmmix.gif)](https://playmmix.2ad.com)

Write, run and single-step MMIX in the browser, on a desktop or a phone,
nothing to install.

**[Try it: playmmix.2ad.com](https://playmmix.2ad.com)**

## On your phone

playmmix is a full assembler and debugger that fits a phone. The run
controls leave the header for a bar pinned to the top of the screen; on a
full-screen iPad that bar sits at the bottom instead. The editor, output and
machine panes stack in one column rather than side by side.

<img src="docs/img/playmmix-phone.png" alt="playmmix on an iPhone, paused mid-run: the control bar pinned at the top, the editor above the machine pane" width="300">

## Examples to try

Each program below is complete: paste it in as is, or click its link to open
it already loaded. Every one prints its answer and halts.

### Say hello

Prints a greeting. Change the name on its own line and run it again.

```asm
% Say hello. Change the name on the next line, then run it.
        LOC     Data_Segment
        GREG    @
Hello   BYTE    "Hello, ",0
Name    BYTE    "world",0
Bang    BYTE    "!",10,0

        LOC     #100
Main    LDA     $255,Hello
        TRAP    0,Fputs,StdOut
        LDA     $255,Name
        TRAP    0,Fputs,StdOut
        LDA     $255,Bang
        TRAP    0,Fputs,StdOut
        SET     $255,0
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=nY7NDoIwEITvPMWKemtMNeEuf8JBxQAXT6YJDZCUYrBEfXvZakCiB2Mvu51vJzNzSNgdCi5EvQC3YDLnoAoOklUcavnc-U2BKCUn-JXQtBJKtTDg9baRq6fHFDslPK-4VD0MYj_AuTZCDOk255j6qJhaIGASauwx7p1d60ZkSJyu0ohMTLKkHfiIny4pNXaslFr0bC3OVpZFdFB_n8b2ASclm3OrLiRRWdQOhUdO7PWXEWv_akz8dDDSL66QCdWBBw)**

```
Hello, world!
```

### Is 13 prime?

Checks whether `N` is prime by trial division, printing the answer and, for
a composite `N`, the divisor that proved it. Change `N` — try a large one.

```asm
% Is N prime? Change N below, 0 to 4294967295.
N       IS      13

        LOC     Data_Segment
        GREG    @
NValue  OCTA    N
Digits  BYTE    0,0,0,0,0,0,0,0,0,0,0
IsPrimeMsg      BYTE " is prime",10,0
NotPrimeMsg     BYTE " is not prime",10,0
NotPrimeColon   BYTE " is not prime: ",0
DividesMsg      BYTE " divides it",10,0

        LOC     #100
% Load N: too wide for a plain SET (over 65535), so it lives in the
% data segment and LDO loads it in one go.
Main    LDA     $1,NValue
        LDO     $1,$1,0         % $1 = N

% Below 2, nothing divides evenly, so N is not prime by definition.
        CMP     $4,$1,2
        BN      $4,NotPrimeSmall

% Try each d from 2 upward. If d divides N evenly, N is composite;
% once d*d exceeds N, no divisor turned up and N is prime. Trying
% past sqrt(N) can never find a new factor, so that is where to stop.
        SET     $2,2            % $2 = d, the trial divisor
TrialLoop
        MUL     $3,$2,$2        % $3 = d*d
        CMP     $4,$3,$1
        BP      $4,PrimeCase    % d*d > N: no divisor found, N is prime
        DIVU    $5,$1,$2
        GET     $6,rR           % rR = N mod d
        BZ      $6,CompositeCase
        ADDU    $2,$2,1
        JMP     TrialLoop

% Print N as decimal text: divide by 10 repeatedly, taking each
% remainder as a digit, and fill the buffer from its end backward
% since the last digit produced is the ones place.
PrimeCase
        SET     $10,$1
        LDA     $11,Digits
        ADDU    $11,$11,9
1H      DIVU    $12,$10,10
        GET     $13,rR
        ADDU    $13,$13,'0'    % '0'..'9' from a 0-9 remainder
        STBU    $13,$11,0
        SUBU    $11,$11,1
        SET     $10,$12
        BNZ     $10,1B
        ADDU    $11,$11,1
        SET     $255,$11
        TRAP    0,Fputs,StdOut
        LDA     $255,IsPrimeMsg
        TRAP    0,Fputs,StdOut
        JMP     Done

% Same idea: print N, then a colon and the divisor $2 found above,
% the same digit-by-digit way.
CompositeCase
        SET     $10,$1
        LDA     $11,Digits
        ADDU    $11,$11,9
2H      DIVU    $12,$10,10
        GET     $13,rR
        ADDU    $13,$13,'0'
        STBU    $13,$11,0
        SUBU    $11,$11,1
        SET     $10,$12
        BNZ     $10,2B
        ADDU    $11,$11,1
        SET     $255,$11
        TRAP    0,Fputs,StdOut
        LDA     $255,NotPrimeColon
        TRAP    0,Fputs,StdOut
        SET     $10,$2
        LDA     $11,Digits
        ADDU    $11,$11,9
3H      DIVU    $12,$10,10
        GET     $13,rR
        ADDU    $13,$13,'0'
        STBU    $13,$11,0
        SUBU    $11,$11,1
        SET     $10,$12
        BNZ     $10,3B
        ADDU    $11,$11,1
        SET     $255,$11
        TRAP    0,Fputs,StdOut
        LDA     $255,DividesMsg
        TRAP    0,Fputs,StdOut
        JMP     Done

% N below 2: print N and that it is not prime, with no divisor to name.
NotPrimeSmall
        SET     $10,$1
        LDA     $11,Digits
        ADDU    $11,$11,9
4H      DIVU    $12,$10,10
        GET     $13,rR
        ADDU    $13,$13,'0'
        STBU    $13,$11,0
        SUBU    $11,$11,1
        SET     $10,$12
        BNZ     $10,4B
        ADDU    $11,$11,1
        SET     $255,$11
        TRAP    0,Fputs,StdOut
        LDA     $255,NotPrimeMsg
        TRAP    0,Fputs,StdOut

Done    SET     $255,0          % a clean exit code, not a stray one
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=1VZtb9pIEP7uXzHKJUpbOZZtICdyupcAuZQTcaJAKl2_VIt3DVbNrutdQvj3nfErLlSKqjRSCRLx7uyzM8_zzHpPYKwhgDSLV-JvGC6ZXAh8notEbWxwwSjo-v1u__x3v99zrACKz3ha_HodyyqHYHI7zH9HzLBPU7FYCWnqyev7q2v6_ccKPrBkLQBuh7NLGgmsUbyIjQYY_D-7ohHXPvBnjfUdJXmjFwViHn0EsS6SP7I9igqUaYU1UVKZg5FDlSh5OPICjjBwFD_GXOhvN-bFMMSmBNwj4jfPda0TmCjGIbhAKhVscAlEKgMGacJiCdOrGbxRjyKD816v03trg1YICUn8SNgSzFIgBkdOQRecApMcJqNbSBCY9qcwJQUslGPdECjlMMrJhWPPLghvssOV5Qx-3WoYTnAE_kQ5cLsByQ--TVQsY7moixWPQibbPMmgRRXMt8BFFMvYxEo69W7Dm7tity7t5tfjg9JJOF4JMV2xJKHdZ9kWBAuXwCHK1Ap8WKcblnEHxhGOVbkEdTZ5KqFapUrHRvyBEEqGAvg7DuIpFAJZCqiWfKlG9s06k4IjbM5lUJvIob2xXkRImTagv2TmTfAWQiZBClIJK-QonhQbiFhoVJZzYZbMEMhmKTJBPaONShsSSOS8WN_2YeeDlPtIObdJZTBZzJIqRWtGTxOl0hrl5mFSoHRsBDr2d1A6hPKOH2Qdo72G9rua9sL8TIsCg8j6i2y6Q1Ok1pLbO_zUOKPxh4ccp0eyHje6XlelntvZfatUfER3wUqhgk0-H6EKH1b6UU51wOVo9FBSh9-mkP_KChuaUDMsCdsjAKbRi2GMfgIjnsxFaRnyqOdCJlLBjODkHMM-k73Jbbg-EytsH44yIwLDVXgu2blFojhJcpHm6ygiG5Av6dASODln4WfyJyLomIxHgQn5J0dA6hRfh-g3pJGmsFc1tX8oHKtWYd8seKjsKNc0tGcXB-Y-Rx61tGf3Le_9NzJ5SB_iee6-UF4HlTqARb7p2KfuaSEf_uM4p_3TonQG7lm_4atJfjbYWY7HSzPzMGgl6X2n4N0j4mM97g2-W-0BIL9HtmwmZveXd8WL5d90bbQ9Nfx2bfappXXNa-a5yysrjlBXcuGU4XGIdmMX1DRkyLy_JZIW5u8aMhT5oGozbOW804DN8U1gIwTNaoLJDXQ2354VTtqwrWMdbpSXsI3_krb56Z7wX80TravCcxFaJfg_JkjnlxKk82qCNFeyH23S8ooLft2kZVvSi9y0rjY23trMsnV9UCCxOx2rfXV5yVbs_lLKd1-9FZ8hvUVa723s7t5J8EROBN7txBNqHiou8hsvDmuTsS29qA9s8p4lBpn7Cg)**

```
13 is prime
```

### What day is my birthday?

Prints the weekday for `DAY`/`MONTH` in every year from `FROM` for `YEARS`
years — the default explores every leap-day birthday from 2000 to 2019.
Change any of the four constants.

```asm
% What day is my birthday? Change MONTH, DAY, FROM and YEARS below.
MONTH   IS      2         % 1-12
DAY     IS      29         % 1-31
FROM    IS      2000         % 1583-9999, the first year
YEARS   IS      20         % 1-100, how many years to print

        LOC     Data_Segment
        GREG    @
MonthDays       BYTE 31,28,31,30,31,30,31,31,30,31,30,31
SakamotoT       BYTE 0,3,2,5,0,3,5,1,4,6,2,4
Sep     BYTE    " ",0
NL      BYTE    10,0
YearDigits      BYTE 0,0,0,0,0,0
Sunday  BYTE    "Sunday",0
Monday  BYTE    "Monday",0
Tuesday BYTE    "Tuesday",0
Wednesday       BYTE "Wednesday",0
Thursday        BYTE "Thursday",0
Friday  BYTE    "Friday",0
Saturday        BYTE "Saturday",0

        LOC     #100
% Load the plain (non-leap) day count for MONTH from a 12-entry table.
Main    LDA     $20,MonthDays
        LDBU    $1,$20,(MONTH-1)

% IsFeb ($2) is 1 when MONTH is February, else 0.
        SET     $7,MONTH
        CMP     $8,$7,2
        SET     $2,0
        BNZ     $8,1F
        SET     $2,1
1H

% YearAdjust ($3) is 1 when MONTH is January or February: Sakamoto's
% method treats those two months as the tail of the PREVIOUS year.
        CMP     $8,$7,3
        SET     $3,0
        BNN     $8,2F
        SET     $3,1
2H

% Sakamoto's per-month offset for MONTH, from a second 12-entry table.
        LDA     $21,SakamotoT
        LDBU    $4,$21,(MONTH-1)

        SET     $5,FROM
        SET     $6,FROM+YEARS
        SET     $23,400         % an immediate operand holds only 0-255

YearLoop
        CMP     $8,$5,$6
        BNN     $8,LoopDone

% Is $5 (the current year) a leap year? Gregorian rule: divisible by
% 4, except a century year (divisible by 100) unless also divisible
% by 400.
        DIVU    $9,$5,4
        GET     $10,rR
        BNZ     $10,NotLeap
        DIVU    $9,$5,100
        GET     $10,rR
        BNZ     $10,IsLeap
        DIVU    $9,$5,$23
        GET     $10,rR
        BNZ     $10,NotLeap
IsLeap  SET     $11,1
        JMP     LeapDone
NotLeap SET     $11,0
LeapDone

% Does DAY exist in MONTH this year? Only February's count moves,
% and only on a leap year.
        SET     $12,$1
        BZ      $2,SkipLeapAdjust
        BZ      $11,SkipLeapAdjust
        ADDU    $12,$12,1
SkipLeapAdjust
        SET     $13,DAY
        CMP     $14,$13,$12
        BP      $14,SkipYear

% Sakamoto's algorithm: weekday = (y + y/4 - y/100 + y/400 + t + day)
% mod 7, y adjusted for Jan/Feb, weekday 0 = Sunday.
        SET     $15,$5
        BZ      $3,NoYearAdjust
        SUBU    $15,$15,1
NoYearAdjust
        DIVU    $16,$15,4
        DIVU    $17,$15,100
        DIVU    $18,$15,$23
        ADDU    $19,$15,$16
        SUBU    $19,$19,$17
        ADDU    $19,$19,$18
        ADDU    $19,$19,$4
        SET     $13,DAY
        ADDU    $19,$19,$13
        DIVU    $9,$19,7
        GET     $22,rR

% Print the year as decimal text, the same digit-by-digit way as the
% prime example: divide by 10 repeatedly, filling the buffer from
% its end backward.
        SET     $30,$5
        LDA     $31,YearDigits
        ADDU    $31,$31,4
3H      DIVU    $32,$30,10
        GET     $33,rR
        ADDU    $33,$33,'0'
        STBU    $33,$31,0
        SUBU    $31,$31,1
        SET     $30,$32
        BNZ     $30,3B
        ADDU    $31,$31,1
        SET     $255,$31
        TRAP    0,Fputs,StdOut
        LDA     $255,Sep
        TRAP    0,Fputs,StdOut

% $22 is the weekday index, 0 for Sunday up to 6 for Saturday. Walk
% it down by one each time it misses, until it names its weekday.
        BZ      $22,PrintSunday
        SUBU    $22,$22,1
        BZ      $22,PrintMonday
        SUBU    $22,$22,1
        BZ      $22,PrintTuesday
        SUBU    $22,$22,1
        BZ      $22,PrintWednesday
        SUBU    $22,$22,1
        BZ      $22,PrintThursday
        SUBU    $22,$22,1
        BZ      $22,PrintFriday
        JMP     PrintSaturday
PrintSunday
        LDA     $255,Sunday
        JMP     PrintWeekdayDone
PrintMonday
        LDA     $255,Monday
        JMP     PrintWeekdayDone
PrintTuesday
        LDA     $255,Tuesday
        JMP     PrintWeekdayDone
PrintWednesday
        LDA     $255,Wednesday
        JMP     PrintWeekdayDone
PrintThursday
        LDA     $255,Thursday
        JMP     PrintWeekdayDone
PrintFriday
        LDA     $255,Friday
        JMP     PrintWeekdayDone
PrintSaturday
        LDA     $255,Saturday
PrintWeekdayDone
        TRAP    0,Fputs,StdOut
        LDA     $255,NL
        TRAP    0,Fputs,StdOut

SkipYear
        ADDU    $5,$5,1
        JMP     YearLoop

LoopDone
        SET     $255,0          % a clean exit code, not a stray one
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=nVfpTuNIEP7vpyjNgiZoOoyPhGOk1SxMuEZcIjCI_bPqxB3ixXZHdntC3n6run3GDmgJMon7q_pcXVeXt-FxzhX4fAVBCtEKJkGi5nj7HX7Mefws4Orm-v6cwejoicHp3c0V8NiHp5OjuzFMRCiXu5aWAICLMeiPC8VnG5y-41qoq29LgcOGhOdYmrguYdt2XWZ44PUP8cNAzQXMgiRVsBI8sYwhdcXmw22bwVwuIeLxSmukoCQskiBWllVIXt780N8jrvg_Y_EcCUQL8Ozu5Iy-_7KuZKzmI75Kc-T46f4EPIe5Bwz_e3btf-PWGvMXHkkl7-uKiDCXDRl9D5nDBmwP7wfWWCwqIfx8gk_Mtq4vobHq2Lj4hPsZBc-BShu85Z81zmKKbMVlFogQN9OEzAJB95lICSuhfIGwR-HHBq098lO5qtXnWVKTyEWKVZI4TYLms80CQWOusqSlXKySRCtqf2CQrW24lNzX2bEIeRBDL5ZxPxR8saOTeyqzWMFMJiafYZbICDg4bh9jnaxA8UkoMJVJlbhHR5p7y7VZGfbqyaPjB406jAR6mrLv7FhoxkV6KibQ23J3qKAcWM5FnD8T7xFLMp6sGIgwFWDvlpzjE5MdW_tMS5fAj6tbAxwwxNy2gotOKd11_Xch7Jx2iTqWc05mUuoc-f9mWEe9La_T1p88JlMBfVaY_Q2KVP6cIkkk1Fyi0xPBMQXxN25JLSVE5LEUeKrjoXgQgpzp37d3J78ubh7GuhR3N-zRaxvuNfZ4XQi7p12ijuXqPVa2wkIkfW0VGjJLRS0TWJEKqZhiBbQyoop5kREOK-u5nREDRgK1jGjZN2TU7Nrre3r9i25oHZHz2KDREnkMQRQJP-BKgMT9UVuey9BPQcbhCuy-OxxaukVcSrnodPWQbe11uZUURjIWJp_RZOhR8KZZkqBvdOx20GNUXfrmO5wl4lkmARqVZKH4Bn7wO0gDdCBMVkgywHx_nYqFQq0pUmSJacbQqwtiU7N3IItDkWLyhKmsaJADcfRAFZDRxS_j8kPayKDq14XLsEUmd-3KwOVrqS7R9g1U1E7-B9lF-gYXxu0jhhnOWvgdB7O60PiZh5BkdJhytYa4bZUwOm8kRUpHOIYhwJIPijJXcyx0E8Ibypqi0LFkTMeM5G-RMmtbn_o6sWRcD31HA3NctlUZe2x2R71n_BIsyCjTd9oSaPUGkaPR6KHipi62QbAywmO43XbWO1igiCFJ9fxbKDGipZJZax88pOxW8-gbLIV4oQPlT-it4Ausvg6gj_8xacyd_lZ4odAO9UhskPsMVsC1pcLXrQd761f0NSvpbCQ0h3OXRzGRhm1_eZgvVR-v1B6K0wnV8HKsTrEyUZ09LTboQPYNQa0gKuxAY_UEr6J0aDBnr8Mowuja36BH18FmbPBurNt0Xmd1IrTfLk7XpeLEwN3SjKgPLd2p8DDzxTSIeAhKvCoziKY8EtikcALrT1Z9_QOWGE1z8iEJDpooIV55tCjaop-3OkjEAs9N4Yc4DcyCMAziZ006yWYzkehjCRlouBNYexM-fVnyxO9IDxwya-lRHlQ4flYDYts7CNM1sLzzNd94WGTI6XS0Qc-rt66KCysKr8_258q6--Ma5tTO7zIVcguc7h15brtF0jh9vHErHUR4BhJUAvd3R7rabXa6yFTKxsq_yVTHIY96OIi_p4fxwYShYYkCV9RyEPvilWFJU6XnI3i2oPeOPbOUT7O78MjDFx1j8OUyprzAfg2CT-egKHEQiII0xQ6Mx6LCMQoXYsy5VKdF_rzdjl7rMp2-5uFtzyNOl7NZ07wNfEQzf1n4iGr5GvGh5-YvGB_RNe8frQPW-DCPltXl0WbCNLEGy6OJlT6PuxzcIFrD3iZa93eDaR18m6rt_wZZG37HsvWINE1bR98mWwtRg-qt8LWIynh2x7AR7bryR1rI9eW7HaScOFpdbaiH0dauyoneKsf0zrZXvSzQ2wJMcWSLafxTONn5gkEsaRpPVcJ11-kw9JyHCvv2fw)**

```
2000 Tuesday
2004 Sunday
2008 Friday
2012 Wednesday
2016 Monday
```

### Digits of pi

Computes `DIGITS` decimal digits of pi with Machin's formula, arbitrary
precision arithmetic on arrays of registers standing in for one huge number.
Change `DIGITS`, up to 1000.

```asm
% Digits of pi. Change DIGITS below, 1 to 1000.
%
% Machin's formula, pi = 16*arctan(1/5) - 4*arctan(1/239), turns pi
% into two series a computer can sum exactly. Each number below is
% too wide for one register, so it lives in memory instead: an array
% of W words, base 1000000000 (nine decimal digits per word), most
% significant word first. DivSmall, MulSmall, AddInto and SubInto
% below are long division, multiplication, addition and subtraction
% on that array, one word at a time -- the same arithmetic taught on
% paper, carried out word by word instead of digit by digit.
DIGITS  IS      100
GUARD   IS      15
TOTALD  IS      DIGITS+1+GUARD
W       IS      (TOTALD+8)/9
LASTOFF IS      8*(W-1)

        LOC     Data_Segment
        GREG    @
BaseConst       OCTA 1000000000
Pow10   OCTA    100000000,10000000,1000000,100000,10000
        OCTA    1000,100,10,1
PowerArr
        LOC     @+8*W
TermArr
        LOC     @+8*W
SumPosArr
        LOC     @+8*W
SumNegArr
        LOC     @+8*W
Result5Arr
        LOC     @+8*W
DecBuf
        LOC     @+DIGITS+2
Lead    BYTE    "3.",0

        LOC     #100
Main    JMP     MainStart

% ---- ZeroArray(addr=$0): set W words at $0 to zero.
ZeroArray
        SET     $2,0
        SET     $3,W
ZL      BZ      $3,ZeroDone
        SET     $4,0
        STO     $4,$0,$2
        ADDU    $2,$2,8
        SUBU    $3,$3,1
        JMP     ZL
ZeroDone
        POP     0,0

% ---- CopyArray(dest=$0, src=$1): dest := src, W words.
CopyArray
        SET     $3,0
        SET     $4,0
CL      CMP     $6,$3,W
        BNN     $6,CopyDone
        LDO     $5,$1,$4
        STO     $5,$0,$4
        ADDU    $4,$4,8
        ADDU    $3,$3,1
        JMP     CL
CopyDone
        POP     0,0

% ---- IsZero(addr=$0): returns 1 if all W words are zero, else 0.
IsZero  SET     $3,0
        SET     $4,W
IZL     BZ      $4,IsZeroYes
        LDO     $5,$0,$3
        BNZ     $5,IsZeroNo
        ADDU    $3,$3,8
        SUBU    $4,$4,1
        JMP     IZL
IsZeroYes
        SET     $0,1
        JMP     IsZeroRet
IsZeroNo
        SET     $0,0
IsZeroRet
        POP     1,0

% ---- DivSmall(addr=$0, divisor=$1): addr /= divisor, W words,
% most-significant word first, remainder carried into the next word.
DivSmall
        LDA     $7,BaseConst
        LDO     $7,$7,0
        SET     $2,0
        SET     $3,0
        SET     $6,W
DSL     BZ      $6,DivDone
        LDO     $5,$0,$3
        MUL     $2,$2,$7
        ADDU    $2,$2,$5
        DIVU    $5,$2,$1
        GET     $2,rR
        STO     $5,$0,$3
        ADDU    $3,$3,8
        SUBU    $6,$6,1
        JMP     DSL
DivDone POP     0,0

% ---- MulSmall(addr=$0, multiplier=$1): addr *= multiplier, W words,
% least-significant word first, carry propagated toward the front.
MulSmall
        LDA     $7,BaseConst
        LDO     $7,$7,0
        SET     $2,0
        SET     $3,W
        SET     $4,LASTOFF
MSL     BZ      $3,MulDone
        LDO     $5,$0,$4
        MUL     $6,$5,$1
        ADDU    $6,$6,$2
        DIVU    $2,$6,$7
        GET     $5,rR
        STO     $5,$0,$4
        SUBU    $4,$4,8
        SUBU    $3,$3,1
        JMP     MSL
MulDone POP     0,0

% ---- AddInto(dest=$0, src=$1): dest += src, W words, least-
% significant word first.
AddInto LDA     $9,BaseConst
        LDO     $9,$9,0
        SET     $2,0
        SET     $3,W
        SET     $4,LASTOFF
AIL     BZ      $3,AddDone
        LDO     $5,$0,$4
        LDO     $6,$1,$4
        ADDU    $7,$5,$6
        ADDU    $7,$7,$2
        CMP     $8,$7,$9
        SET     $2,0
        BN      $8,AddNoCarry
        SUBU    $7,$7,$9
        SET     $2,1
AddNoCarry
        STO     $7,$0,$4
        SUBU    $4,$4,8
        SUBU    $3,$3,1
        JMP     AIL
AddDone POP     0,0

% ---- SubInto(dest=$0, src=$1): dest -= src (dest >= src assumed),
% W words, least-significant word first.
SubInto LDA     $9,BaseConst
        LDO     $9,$9,0
        SET     $2,0
        SET     $3,W
        SET     $4,LASTOFF
SIL     BZ      $3,SubDone
        LDO     $5,$0,$4
        LDO     $6,$1,$4
        ADDU    $6,$6,$2
        CMP     $8,$5,$6
        SET     $2,0
        BNN     $8,SubNoBorrow
        ADDU    $5,$5,$9
        SET     $2,1
SubNoBorrow
        SUBU    $5,$5,$6
        STO     $5,$0,$4
        SUBU    $4,$4,8
        SUBU    $3,$3,1
        JMP     SIL
SubDone POP     0,0

% ---- Sum arctan(1/x) * 10^(9*W-1), x's square in $1, into SumPosArr
% minus SumNegArr, alternating sign, stopping once the shrinking power
% underflows to zero.
MainStart
        LDA     $9,SumPosArr
        PUSHJ   $8,ZeroArray
        LDA     $9,SumNegArr
        PUSHJ   $8,ZeroArray
        LDA     $9,PowerArr
        PUSHJ   $8,ZeroArray
        LDA     $9,Pow10
        LDO     $9,$9,0
        LDA     $10,PowerArr
        STO     $9,$10,0
        LDA     $9,PowerArr
        SET     $10,5
        PUSHJ   $8,DivSmall

        SET     $0,1            % $0 = 2k+1, the term's odd denominator
        SET     $1,0            % $1 = 0 add to SumPos, 1 add to SumNeg
Series5Loop
% Power has shrunk to nothing: every later term would add zero at
% this precision, so the series is done.
        LDA     $9,PowerArr
        PUSHJ   $8,IsZero
        BNZ     $8,Series5Done
% This term is Power divided by (2k+1); dividing a copy keeps Power
% itself intact for the next iteration.
        LDA     $9,TermArr
        LDA     $10,PowerArr
        PUSHJ   $8,CopyArray
        LDA     $9,TermArr
        SET     $10,$0
        PUSHJ   $8,DivSmall
% Machin's series alternates sign: add this term to whichever running
% total is due next.
        BNZ     $1,Series5Neg
        LDA     $9,SumPosArr
        LDA     $10,TermArr
        PUSHJ   $8,AddInto
        JMP     Series5AfterAdd
Series5Neg
        LDA     $9,SumNegArr
        LDA     $10,TermArr
        PUSHJ   $8,AddInto
Series5AfterAdd
% Shrink Power by 5^2 and move to the next odd denominator and sign,
% ready for the next term.
        LDA     $9,PowerArr
        SET     $10,25
        PUSHJ   $8,DivSmall
        ADDU    $0,$0,2
        SET     $2,1
        SUBU    $1,$2,$1
        JMP     Series5Loop
% SumPos minus SumNeg is arctan(1/5); keep it aside in Result5Arr
% before reusing these same arrays for the 1/239 series below.
Series5Done
        LDA     $9,SumPosArr
        LDA     $10,SumNegArr
        PUSHJ   $8,SubInto
        LDA     $9,Result5Arr
        LDA     $10,SumPosArr
        PUSHJ   $8,CopyArray

        LDA     $9,SumPosArr
        PUSHJ   $8,ZeroArray
        LDA     $9,SumNegArr
        PUSHJ   $8,ZeroArray
        LDA     $9,PowerArr
        PUSHJ   $8,ZeroArray
        LDA     $9,Pow10
        LDO     $9,$9,0
        LDA     $10,PowerArr
        STO     $9,$10,0
        LDA     $9,PowerArr
        SET     $10,239
        PUSHJ   $8,DivSmall

% arctan(1/239), the same series as above with x=239: 239^2=57121.
        SET     $0,1
        SET     $1,0
Series239Loop
        LDA     $9,PowerArr
        PUSHJ   $8,IsZero
        BNZ     $8,Series239Done
        LDA     $9,TermArr
        LDA     $10,PowerArr
        PUSHJ   $8,CopyArray
        LDA     $9,TermArr
        SET     $10,$0
        PUSHJ   $8,DivSmall
        BNZ     $1,Series239Neg
        LDA     $9,SumPosArr
        LDA     $10,TermArr
        PUSHJ   $8,AddInto
        JMP     Series239AfterAdd
Series239Neg
        LDA     $9,SumNegArr
        LDA     $10,TermArr
        PUSHJ   $8,AddInto
Series239AfterAdd
        LDA     $9,PowerArr
        SET     $10,57121
        PUSHJ   $8,DivSmall
        ADDU    $0,$0,2
        SET     $2,1
        SUBU    $1,$2,$1
        JMP     Series239Loop
Series239Done
        LDA     $9,SumPosArr
        LDA     $10,SumNegArr
        PUSHJ   $8,SubInto

% pi = 16*arctan(1/5) - 4*arctan(1/239): scale each arctan, then
% combine the two into Result5Arr.
        LDA     $9,Result5Arr
        SET     $10,16
        PUSHJ   $8,MulSmall
        LDA     $9,SumPosArr
        SET     $10,4
        PUSHJ   $8,MulSmall
        LDA     $9,Result5Arr
        LDA     $10,SumPosArr
        PUSHJ   $8,SubInto

% ---- Extract DIGITS decimal digits after the point from Result5Arr
% (the leading '3' is the literal below) and print them. Result5Arr
% holds pi's digits nine to a word, so digit $1 sits in word $1/9 at
% position $1 mod 9 within it -- Pow10 picks that one digit out.
        SET     $1,1
        SET     $2,DIGITS
        LDA     $3,DecBuf
DigitLoop
        BZ      $2,DigitsDone
        DIVU    $4,$1,9         % $4 = word index, rR = position in it
        GET     $5,rR
        SET     $16,8            % OCTA entries are 8 bytes apart
        MUL     $6,$4,$16
        LDA     $7,Result5Arr
        LDO     $8,$7,$6        % $8 = that word's nine digits
        MUL     $15,$5,$16
        LDA     $9,Pow10
        LDO     $10,$9,$15      % $10 = 10^(8 - position)
        DIVU    $11,$8,$10
        SET     $12,10
        DIVU    $13,$11,$12
        GET     $14,rR          % rR = the single digit wanted
        ADDU    $14,$14,'0'
        STBU    $14,$3,0
        ADDU    $3,$3,1
        ADDU    $1,$1,1
        SUBU    $2,$2,1
        JMP     DigitLoop
DigitsDone
        SET     $14,10
        STBU    $14,$3,0

        LDA     $255,Lead
        TRAP    0,Fputs,StdOut
        LDA     $255,DecBuf
        TRAP    0,Fputs,StdOut
        SET     $255,0
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=7VpZc-JIEn7Xr8iYxeFLpiUM-JjwxmDT3eMJHx0Gr2P6oTdkVIDCoGJ1-Nhfv19WSULowHasd2cehqBboo6sysyv8ipvUN-beFFIckwLr0lnU8efCOqffz0fDuhezOSTSTZFkmzLsprGhrFBl85o6vmbIY1lMI9njomZdEJ2d8cJRpHjb9mfOtu0R-3l79b-0bZJURz4IQaDhueDZPQkKRSBJ0JyaCTnizgSAY0cn8J4TuLZGUWzlyZ9xnrkx_N7dKodkReCRCQlPXmu4G2Q9AUFYuKFoGBSKMmLaOY9grLn01zMZfCCN_Q67jFhAScInBcQAdt39CQDNzTp3gmFYlN_aMv3QNUVI2_uzMjVclpgEzwe7MxlGIFE6E18b-xh25HqobEXhFETgn0cYOLMpMt4lrz1XPecOXd8lwbxPb-DgmbKCQTNpD_BSo9e6EkfK8SzyFvMQDtSvx3X9fhNzQ_j-yiAiPCbGfEpmjqRZsxU8lCb4SaKvLmgvT2MEBQ6eHcCL5rOReSNKHLiyTQiRWThLFh8IxDxhEsyTji6f9HPRIIsNSUO7lAvTSNBDJ3jH38gR-Prbe-mT7m2jjG8HvYu-ssmPW3X3lVjjTvdmnVv6fG7h9ufjoyL3mB4_eVL1nm4s3W3Z28bRjKJLq7PNFEncv45EJO58KOs8-vN56_8_MU4hZ7PJFhJeq7Phr2c3o1v8sm20nbNiv6YduEleepHtlJ-Hnfhn2kzVRH0gqC02192D3fujKEI5vW9g3j-TYZr-6_EpL7_RoSAUqd-QF-MTuNxRWeioJZxwYrH5_T34Wd-_rTf_Mm0ysL_G2v-0sGxw-e3y2-qkX8PIieIDKBsDx_6LgLZY6xuAdTBScPaPoYtiNLTyLhtWGx3_o2BTSMbnq03-DxUz0bLtMqN--ad8f1CN55-p7SRyfRxNsoT2nkqw-u0sWGZjVbW0ev3b5M18T1cTrg9vU1WwNfO2lP2v18YpZW_Xes-i4WYCOVMLl60UFwRRhAKTFkwOmnYEA630PEJN5iplJpGNqNKBFY1m2eJXM6S3TW6ppJXOvj06iptZ_Iru77oJ6LpmA3bbLTLMusombXLMoMw2zmZZe01Mju7MEqrV8nsPGTR5lAUCO1nbPLGBKu7hBQMLKPJJDGDpYc303Nfl9mdcZ6AKcNS29STfxdhpXAgg_2cRL-nHXrWlayRQwWmlNzK8sGOjPIWsk1bVVPU8BsRGaVd5OZZxnJcUfB2TvCpf0tFb2rHJQONV26lTydpY4ZZE_PZc-5V-00T-pvDWrgqEtB-SAcL8F2-eNZj4W-S1XPC72kODszMwJc1c2Dia73ZiFQ0doGG_qCAhq6J_dQelBUsXN5e0NKGNA5qjEujk3X0z_9xm5JCx1KvX5e7D27qTuL-25EGO9CtgA24NRL2Kk9gGt4sgZDGLSKPhZ2TXPsKHGbCWYMHRsELLQK5cCZOBDhE8slBNwNiHEgfwUe6g_8tGO6qTEMSlBiXRUjsm9jVOki0y5CAAjp5BWcaU5rJOaIMEi3VcVCGRGcNJNo1RubtDg3cGgl7lZBI4tw6N7a76sbMBAH1wbSRBs6ZYo_WKfbIxPeDFNs7LykWm3mbYrOObsFbZoo9UBrvVnYc5DWeOetD1XG0nrnTK0pHY7NX8ozPUFm7B_W0bKNq4nB5cj4ERxCukUizEkdJjlSHoz2FI1Ld9Hf9wwmRPArkZ6BRwFcduJJV_gBwDcrgwmY-ClxFq5HH0ArqajB0lY7Gnq7kqQwC-VRepaOI1YCoamYGik5xGx9tpiBeI5FnDbzmlNUonrdpBznbj62jHU4rTXpGiSP8V8xhI7IZyFgHIstcDIGM58chZdkXMvQZChA-0nXk8Qw3IDaSiwX_lP5I6Ax8Gnj-AzctOCcEmZjDnTGKAOEy41mmTCWndmSW88Fvt4Nff9PaKudKqzMLmeJbZ5YS2HdMtK1Xz1E2AdlyaakMGZhkW1WzKvaXAREzOlW7zmLIyvCZcp8NTkZPqPWwCxSwDqHlOeAhXReWyJfAgRPJqqVNq0DHBh2LIyLKsMQVtmUD1GMMVFWscyHlAvBQnNHUCRk6sf_AA30ZoQg3OSbxKBAezRwunfGuYNnimavoMZCQSHOtbOqhdBWglKULS6GOp5PiG_pcHJHme7Wu84RylgODofevDNkGDXl1tTc8NTOcFbhCVZa2WKrbP-smPhVcC1y80IMQi2Q4FwyjUMzGfAJR8lIlvywh8EBaFcgqGSgVVtbhLMdcObNeQzQPtYa1Fmu5-mla-0xsBl7ZZBxrLGQyg7Kfpt5oyoomqB9VyYkqf0aoSrLqYi2HZlkRdqoIxtSbzEheOkUmc9wk4VjZ4OrlemPsHGOMV5cvVq3et3xxuQ0aKNuagAzg6vxoqWLpXD4KymeRhZOrK6pssEEkQKnrZRVjrInmu61Oa73ZKblSBo_ZqvakJZ9nF7LBgg4S26FVvOKoGDS5Sv3P6qRxwdwJuZwOX5crF3J1GpLg6noc8umESMKsiozjEWaCUmX-FNSqpt008obg3QBc66vS8nkF1apq5yrZeue5PPZ_-d0P8LtAxHrPu0HFS6L0jiK1jvje8-l9wpUFPZ9g1DHhvx-tk86B3bKb62tfeU-coBGT1eH4YG8HsnUw_7P5oFpHAR7-v64CCxacxdotfIS7yC_57jCSEfeH2fQUuK_i7QPMKt8GvuVWF7c2I2cmSPAlre5RZ5ivE3Gle89XqCpgxl2vSp-Wxrn5RtudV4Ddrdp0ff2vShZ5eu33kvtvfEtOtir9_PysLnDT2_bCTbMzViE9ZLeQkBzXOuernnmLO1HaUFHz5v4mO3bVpCLimfbB2yq0WQRMAp3z5iqNqZy5fCePcDRZV9168x21qpGoXEFf9iJ1CXkAAgRVPWnYn450erGQob6XxpC5dOlIGWuMwyzwqS9UF97oIdSX1JyPa5q4Ym5W2Wq76sBoOZXlvm8m15fqbxlWbHtWWMFsxd7KkcnKqG0uoRxRLk1rA_vJnbcrnnErcYOGjE_F2mtF14ybrnlIK0mguiHG5bR2cAiuDhGrcvzvLPIJf74uzDvsVtW3KwF5nbomrtR1c2wdggulAuZtM1G2Vnx5WVsXaKrWrY0u2PlwsNDJlrQ5c-a6yiGsRyrB7bIObBi_Qw4zKiQIY2lVzEDZh2fZrbIq7DZ0kZe5UqAKLnBaZin-nlALFG7Zatss77a5aW3mYqHTZV_-aqjuJnNJzFxFdGrt1UVPxX1LhuIKyA5yDOZFVdxcWWWtTsfkS_ysZ3jTUwta5pdFHIXmIHKv46h6YuHvA16ZujyzmGpVzPoVOS86_gM)**

```
3.1415926535897932384626433832795028841971693993751058209749445923078164062862089986280348253421170679
```

## A short MMIX primer

MMIX is a register machine: 256 registers, `$0` to `$255`, and a small set
of instructions, one per line. A line has an optional label, an operation
and its operands — `Main LDA $255,Hello` labels the line `Main`, its
operation is `LDA` (load address), and its operands are `$255` and `Hello`.
Comments start with `%` and run to the end of the line.

Walking through the hello example:

- `LOC Data_Segment` and `LOC #100` set where what follows is placed: the
  data segment, for a program's fixed values, and address `#100`
  (hexadecimal), where every playmmix program's code starts.
- `GREG @` allocates a register holding the data segment's current address
  — a bookkeeping line every program with a data segment carries near its
  top.
- `BYTE "Hello, ",0` lays down a string's bytes plus a trailing zero, how
  MMIX marks where a string ends.
- `LDA $255,Hello` loads the address of `Hello`'s bytes into `$255`. `TRAP
  0,Fputs,StdOut` prints the string at that address to standard output.
- `SET $255,0` then `TRAP 0,Halt,0` sets `$255` to zero and stops the
  machine — `$255` doubles as the TRAP argument register and, once halted,
  the exit code the machine pane shows.

A wider constant — anything over 65535 — cannot fit in one `SET`. It sits in
the data segment as an `OCTA` (eight bytes) instead, and `LDO` loads it back
in one instruction: see `NValue` in the prime example above. `IS` names a
constant without storing it anywhere, the way every example's tunable value
is written — `N IS 13`.

That covers what these four examples use. For the rest of MMIX, the
[instruction reference](https://mmix.cs.hm.edu/doc/instructions-en.html) is
the full list.

## A tour of the screen

| region            | shows                                                | updates                                         |
|-------------------|-------------------------------------------------------|------------------------------------------------|
| header            | the title, New, the run controls, the run-state label and status message, and Share | the run-state label and status message, after every action |
| editor            | the MMIX source with a line-number gutter; click a line number to set a breakpoint | live as you type (debounced), and its current-line marker while paused |
| output            | the program's stdout, stderr and diagnostics, in the order they arrived; `exit N` once halted | at each pause, and at every chunk boundary during a run |
| machine status    | the program counter, the call depth, and `exit N` once halted | after every step or run segment |
| registers         | the general-purpose registers in play: the current locals, the current globals, and any register that has ever gone nonzero this load | after every step or run segment, changes highlighted |
| special registers | the CPU state registers — `rA`, `rG`, `rL`, `rJ` and the rest — the same way | after every step or run segment, changes highlighted |
| memory            | the loaded text and data segments in hex and ASCII, aligned to 16-byte rows | after every step or run segment, the current row and instruction highlighted |

## The controls

Run, Continue, Step, Next, Interrupt and Reset sit top left on desktop, each
key cued by its button's own bold amber first letter; on a phone the bar
sits fixed to the top of the screen instead, and to the bottom on a
full-screen iPad.

- **New** — start over from the minimal skeleton, asking first when there
  is work to lose.
- **Run** (`r`) — restart from the start state and run to a breakpoint or
  halt.
- **Continue** (`c`) — resume a paused run in place: execute the instruction
  at the PC, then run to a breakpoint or halt.
- **Step** (`s`, `F11`) — execute one source-level step, following into
  calls.
- **Next** (`n`, `F10`) — execute one source line, running any call along
  the way to completion rather than stepping into it.
- **Interrupt** (`i`) — pause a Run, Continue, or Next in progress.
- **Reset** — reload the current source from the top, clearing output and
  highlights; breakpoints are kept.
- **Share** — put the program into a link and share or copy it, asking
  first when it would replace different work.

Run always restarts from the top, even mid-program, so it never quietly does
nothing; Continue instead picks up exactly where a paused run left off.
Reset also returns to the top, but waits there instead of running.

Press **Ctrl-S** (**Cmd-S** on macOS) anywhere on the page to reassemble
immediately instead of waiting for the debounce.

## Good to know

- The program is kept in this browser as you type. iOS Safari can clear a
  site's storage after days unused, so treat it as a convenience, not
  permanent storage.
- **New** and opening a shared link both ask first when there is different
  work to lose.
- **Share** puts the whole program in the link itself — nothing is stored
  on a server.
- A program can print, but playmmix has no keyboard input.

## Learn MMIX

- [Knuth's MMIX page](https://www-cs-faculty.stanford.edu/~knuth/mmix.html) —
  the introduction, the Fascicle 1 tutorial and the instruction set.
- [mmix.cs.hm.edu](https://mmix.cs.hm.edu/) — full documentation, a visual
  debugger and example programs.

## Build and run locally

### Run it

Install [Trunk](https://trunkrs.dev/) and the wasm target:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

Then, from this directory:

```sh
trunk serve
```

Open `http://127.0.0.1:8080`.

### Debug it

`cargo test` runs the Rust-side unit tests — editor, control, machine-pane
logic — on the host target, no browser required. `docs/layout-spec.md` is
the normative spec for machine-pane layout decisions.

### Deploy it

playmmix deploys to [playmmix.2ad.com](https://playmmix.2ad.com) via CDK;
see [`cdk/README.md`](cdk/README.md).

### Contribute

See [`AGENTS.md`](AGENTS.md) for this project's coding and review
conventions.

## Relationship to checksmix

playmmix uses [checksmix](https://github.com/jac18281828/checksmix) as its
MMIX assembler and interpreter — instruction decoding, the register file
and TRAP handling all live there. playmmix calls only its public API
(`MMixAssembler`, `MMix`) and builds the editor, the run/step controls and
the machine-pane rendering around it.

playmmix requires checksmix 0.3.13 or later and reads MMIXAL exactly as that
release does; see checksmix's own `CHANGELOG.md` for the language rules.

Want MMIX outside a browser — a real debugger, `.mmo` object files,
gdb-style stepping from a shell? [Get checksmix
here](https://github.com/jac18281828/checksmix).
