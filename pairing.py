#!/usr/bin/python -tt
import sys
import csv
import random
import argparse

"""Read csv with names, email addresses and room numbers of participants
    """
def read_file(input_file_path):
    with open(input_file_path) as csvfile:
        reader = csv.reader(csvfile, delimiter=',')
        giver_list = list(reader)
        giver_list = giver_list[1:] # remove first row
        return giver_list

""" Write csv with names, email addresses and room numbers of givers and receivers
    """
def write_file(givers_list, rand_vec):
    with open('pairs.csv', 'w') as csvfile:
        writer = csv.writer(csvfile, delimiter=',')
        for ind in range(givers_list.__len__()):
            writer.writerow([givers_list[ind][1] + ',' + givers_list[ind][2]
                + ',' + givers_list[ind][3]
                + ',' + givers_list[rand_vec[ind]][1] + ','
                + givers_list[rand_vec[ind]][2] + ',' + givers_list[rand_vec[ind]][3]])

""" Define a main() function that calls the necessary functions.
    """
def main():
    parser = argparse.ArgumentParser(description='Secret Santa list')
    parser.add_argument('input_file', type=str, help='Path to the input file')
    args = parser.parse_args()
    input_file_path = args.input_file

    # Get givers list and generate receivers list
    givers_list = read_file(input_file_path) # read csv file
    N = givers_list.__len__() # number of participants
    rand_vec = list(range(N)) # array of indices
    max_iterations = 1000
    iteration = 0
    conditions_met = False
    # brute force randomisation and checking conditions
    while iteration<max_iterations and not conditions_met:
        iteration += 1           # increment iteration number
        random.shuffle(rand_vec) # randomise array of indices
        conditions_met = True    # unless one of following breaks
        for ind in range(N):     # go over all indices of random index vector
            # Condition 1: not to themselves
            if rand_vec[ind] == ind:
                conditions_met = False
                break
            # Condition 2: not to their own Secret Valentine
            elif rand_vec[rand_vec[ind]] == ind:
                conditions_met = False
                break
            # Condition 3: not to 'roommates' (people with same room number listed)
            elif givers_list[ind][2] == givers_list[rand_vec[ind]][2]:
                conditions_met = False
                break
            # Condition 4: not to person X if your roommate has person X's roommate
            roommates = [i for i,g in enumerate(givers_list) if g[2] == givers_list[ind][2] and i is not ind] # people with same room number
            # for roommates of 'ind', check that they do not give to the same 'team'
            for roommate in roommates:
                # If A1 -> B1, then NOT A2 -> B2
                # i.e. check if recepient of ind is not from same room as recipient of roommate
                if givers_list[rand_vec[ind]][2] == givers_list[rand_vec[roommate]][2]: 
                    conditions_met = False
                    break
    write_file(givers_list, rand_vec) # write csv file with pairs
## Uncomment below to test in terminal
#    for ind in range(N): # print in Terminal
#        print(givers_list[ind][1], "(", givers_list[ind][2], ") to", \
#           givers_list[rand_vec[ind]][1], "(", givers_list[rand_vec[ind]][2], ")")
#    print("Number of iterations needed: ", iteration) # Print number of iterations

if __name__ == '__main__':
    main()
